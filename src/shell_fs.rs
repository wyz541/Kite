use std::collections::HashMap;
use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::fs::{DirectorySnapshot, EntryKind, FileEntry, SortSpec, sort_entries};
use futures_channel::oneshot;
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, IBindCtx,
};
use windows::Win32::System::SystemServices::{
    SFGAO_CANCOPY, SFGAO_CANDELETE, SFGAO_CANMOVE, SFGAO_CANRENAME, SFGAO_FOLDER,
    SFGAO_HASPROPSHEET, SFGAO_STREAM,
};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FILEOPERATION_FLAGS, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI,
    FOF_RENAMEONCOLLISION, FOFX_PRESERVEFILEEXTENSIONS, FileOperation, IEnumShellItems,
    IFileOperation, IFileOperationProgressSink, IShellItem, SHCreateItemFromParsingName,
    SHCreateItemFromRelativeName, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SW_SHOWNORMAL};
use windows::core::{PCWSTR, PWSTR};

type ItemCapabilities = (bool, bool, bool);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellCapabilitySnapshot {
    pub exists: bool,
    pub readable: bool,
    pub is_container: bool,
    pub can_copy: bool,
    pub can_copy_path: bool,
    pub can_paste: bool,
    pub can_create: bool,
    pub can_move: bool,
    pub can_rename: bool,
    pub can_delete: bool,
    pub can_properties: bool,
}

static DISPLAY_NAME_CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
static ITEM_CAPABILITY_CACHE: OnceLock<Mutex<HashMap<PathBuf, ItemCapabilities>>> = OnceLock::new();
static SHELL_CAPABILITY_CACHE: OnceLock<Mutex<HashMap<PathBuf, ShellCapabilitySnapshot>>> =
    OnceLock::new();

fn display_name_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    DISPLAY_NAME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn item_capability_cache() -> &'static Mutex<HashMap<PathBuf, ItemCapabilities>> {
    ITEM_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn shell_capability_cache() -> &'static Mutex<HashMap<PathBuf, ShellCapabilitySnapshot>> {
    SHELL_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn clear_shell_caches() {
    display_name_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    item_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    shell_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn cached_display_name(path: &Path) -> Option<String> {
    display_name_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .cloned()
}

fn cache_display_name(path: &Path, name: &str) {
    display_name_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), name.to_string());
}

fn cached_item_capabilities_or_else(
    path: &Path,
    compute: impl FnOnce() -> Option<ItemCapabilities>,
) -> ItemCapabilities {
    if let Some(capabilities) = item_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .copied()
    {
        return capabilities;
    }
    let Some(capabilities) = compute() else {
        return (false, false, false);
    };
    item_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), capabilities);
    capabilities
}

fn item_capabilities_for(item: &IShellItem) -> Option<ItemCapabilities> {
    let attributes =
        unsafe { item.GetAttributes(SFGAO_CANRENAME | SFGAO_CANDELETE | SFGAO_CANMOVE) }.ok()?;
    Some((
        attributes.0 & SFGAO_CANRENAME.0 != 0,
        attributes.0 & SFGAO_CANDELETE.0 != 0,
        attributes.0 & SFGAO_CANMOVE.0 != 0,
    ))
}

fn capability_snapshot_for(
    item: &IShellItem,
    locator_is_namespace: bool,
) -> Option<ShellCapabilitySnapshot> {
    let attributes = unsafe {
        item.GetAttributes(
            SFGAO_CANCOPY
                | SFGAO_CANMOVE
                | SFGAO_CANRENAME
                | SFGAO_CANDELETE
                | SFGAO_FOLDER
                | SFGAO_STREAM
                | SFGAO_HASPROPSHEET,
        )
    }
    .ok()?;
    let has =
        |flag: windows::Win32::System::SystemServices::SFGAO_FLAGS| attributes.0 & flag.0 != 0;
    let is_container = has(SFGAO_FOLDER);
    let can_copy = has(SFGAO_CANCOPY);
    let can_move = has(SFGAO_CANMOVE);
    let can_rename = has(SFGAO_CANRENAME);
    let can_delete = has(SFGAO_CANDELETE);
    Some(ShellCapabilitySnapshot {
        exists: true,
        readable: can_copy || has(SFGAO_STREAM),
        is_container,
        can_copy,
        can_copy_path: locator_is_namespace,
        can_paste: is_container && (can_copy || can_move),
        can_create: is_container && (can_copy || can_move),
        can_move,
        can_rename,
        can_delete,
        can_properties: has(SFGAO_HASPROPSHEET),
    })
}

fn prime_shell_metadata(path: &Path, item: &IShellItem) {
    if let Some(name) =
        display_name_for_item(item, SIGDN_NORMALDISPLAY).filter(|value| !value.is_empty())
    {
        cache_display_name(path, &name);
    }
    if let Some(capabilities) = item_capabilities_for(item) {
        item_capability_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_path_buf(), capabilities);
    }
}

pub(crate) fn is_shell_namespace_path(path: &Path) -> bool {
    let value = path.as_os_str().to_string_lossy();
    value.starts_with("::{") || value.starts_with("shell:")
}

pub(crate) fn item_capabilities(path: &Path) -> ItemCapabilities {
    cached_item_capabilities_or_else(path, || {
        let _com = ComGuard::new().ok()?;
        let item = resolve_item(path).ok()?;
        item_capabilities_for(&item)
    })
}

pub(crate) fn capability_snapshot(path: &Path) -> ShellCapabilitySnapshot {
    if let Some(snapshot) = shell_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path)
        .copied()
    {
        return snapshot;
    }
    let snapshot = (|| {
        let _com = ComGuard::new().ok()?;
        let item = resolve_item(path).ok()?;
        capability_snapshot_for(&item, is_shell_namespace_path(path))
    })()
    .unwrap_or_default();
    shell_capability_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path.to_path_buf(), snapshot);
    snapshot
}

pub(crate) fn exists(path: &Path) -> bool {
    if is_shell_namespace_path(path) {
        capability_snapshot(path).exists
    } else {
        path.exists()
    }
}

pub(crate) fn is_readable(path: &Path) -> bool {
    if is_shell_namespace_path(path) {
        capability_snapshot(path).readable
    } else {
        path.is_dir() || std::fs::File::open(path).is_ok()
    }
}

pub(crate) fn can_rename_item(path: &Path) -> bool {
    item_capabilities(path).0
}

pub(crate) fn can_delete_item(path: &Path) -> bool {
    item_capabilities(path).1
}

pub(crate) fn can_move_item(path: &Path) -> bool {
    item_capabilities(path).2
}

pub(crate) async fn copy_item_with_progress(
    source: PathBuf,
    destination: PathBuf,
    owner: Option<isize>,
) -> Result<(), String> {
    let (sender, receiver) = oneshot::channel();
    // Use a fresh STA for Shell UI without blocking Kite's window thread.
    std::thread::Builder::new()
        .name("kite-shell-copy".into())
        .spawn(move || {
            let owner = owner.map(|handle| HWND(handle as *mut c_void));
            let _ = sender.send(copy_item(&source, &destination, owner));
        })
        .map_err(|error| format!("无法启动复制任务：{error}"))?;
    receiver.await.map_err(|_| "复制任务意外结束".to_string())?
}

fn copy_item(source: &Path, destination: &Path, owner: Option<HWND>) -> Result<(), String> {
    let mut target_name = None;
    if !is_shell_namespace_path(source) && !is_shell_namespace_path(destination) {
        let name = source
            .file_name()
            .ok_or_else(|| "不能复制或移动根目录".to_string())?;
        if !destination.is_dir() {
            return Err("目标文件夹不存在".to_string());
        }
        let metadata = std::fs::symlink_metadata(source)
            .map_err(|error| format!("无法读取源项目：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("暂不支持复制或移动符号链接".to_string());
        }
        if metadata.is_dir() {
            let source_dir = dunce::canonicalize(source)
                .map_err(|error| format!("无法读取源文件夹：{error}"))?;
            let target_dir = dunce::canonicalize(destination)
                .map_err(|error| format!("无法打开目标文件夹：{error}"))?;
            if target_dir
                .ancestors()
                .any(|ancestor| crate::platform::paths_equal(&source_dir, ancestor))
            {
                return Err("不能把文件夹复制或移动到自身内部".to_string());
            }
        }
        let target = crate::fs::unique_destination(destination, name);
        target_name = target
            .file_name()
            .filter(|target_name| *target_name != name)
            .map(|name| {
                name.encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>()
            });
    }

    let _com = ComGuard::new()?;
    let source_item = resolve_item(source)
        .map_err(|error| format!("无法读取源项目 {}：{error}", source.display()))?;
    let destination_item = resolve_item(destination)
        .map_err(|error| format!("无法打开目标文件夹 {}：{error}", destination.display()))?;
    let operation = file_operation_with_flags(
        FOF_NOCONFIRMMKDIR | FOF_RENAMEONCOLLISION | FOFX_PRESERVEFILEEXTENSIONS,
    )?;
    unsafe {
        if let Some(owner) = owner.filter(|&hwnd| IsWindow(Some(hwnd)).as_bool()) {
            operation
                .SetOwnerWindow(owner)
                .map_err(|error| format!("无法设置 Windows 复制窗口：{error}"))?;
        }
        operation
            .CopyItem(
                &source_item,
                &destination_item,
                target_name
                    .as_ref()
                    .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr())),
                None::<&IFileOperationProgressSink>,
            )
            .map_err(|error| format!("无法准备复制：{error}"))?;
    }
    perform_operations(&operation, "复制")
}

pub(crate) fn transfer_items(
    sources: &[PathBuf],
    destination: &Path,
    move_source: bool,
) -> Result<(), String> {
    let _com = ComGuard::new()?;
    let destination_item =
        resolve_item(destination).map_err(|error| format!("无法打开目标移动设备目录：{error}"))?;
    let operation = file_operation()?;
    for source in sources {
        let source_item = resolve_item(source)
            .map_err(|error| format!("无法读取源项目 {}：{error}", source.display()))?;
        let result = unsafe {
            if move_source {
                operation.MoveItem(
                    &source_item,
                    &destination_item,
                    PCWSTR::null(),
                    None::<&IFileOperationProgressSink>,
                )
            } else {
                operation.CopyItem(
                    &source_item,
                    &destination_item,
                    PCWSTR::null(),
                    None::<&IFileOperationProgressSink>,
                )
            }
        };
        result.map_err(|error| format!("无法写入移动设备：{error}"))?;
    }
    perform_operations(&operation, "写入移动设备")
}

pub(crate) fn delete_item(path: &Path) -> Result<(), String> {
    let _com = ComGuard::new()?;
    let item = resolve_item(path).map_err(|error| format!("无法读取设备项目：{error}"))?;
    let operation = file_operation()?;
    unsafe {
        operation
            .DeleteItem(&item, None::<&IFileOperationProgressSink>)
            .map_err(|error| format!("无法删除设备项目：{error}"))?;
    }
    perform_operations(&operation, "删除设备项目")
}

pub(crate) fn rename_item(path: &Path, name: &str) -> Result<(), String> {
    let _com = ComGuard::new()?;
    let item = resolve_item(path).map_err(|error| format!("无法读取设备项目：{error}"))?;
    let operation = file_operation()?;
    let name = wide(name);
    unsafe {
        operation
            .RenameItem(
                &item,
                PCWSTR(name.as_ptr()),
                None::<&IFileOperationProgressSink>,
            )
            .map_err(|error| format!("无法重命名设备项目：{error}"))?;
    }
    perform_operations(&operation, "重命名设备项目")
}

pub(crate) fn create_item_in(
    directory: &Path,
    name: &str,
    is_directory: bool,
) -> Result<(), String> {
    let _com = ComGuard::new()?;
    let folder = resolve_item(directory).map_err(|error| format!("无法打开设备目录：{error}"))?;
    let operation = file_operation()?;
    let name = wide(name);
    let attributes = if is_directory {
        FILE_ATTRIBUTE_DIRECTORY.0
    } else {
        FILE_ATTRIBUTE_NORMAL.0
    };
    unsafe {
        operation
            .NewItem(
                &folder,
                attributes,
                PCWSTR(name.as_ptr()),
                PCWSTR::null(),
                None::<&IFileOperationProgressSink>,
            )
            .map_err(|error| format!("无法在设备中创建项目：{error}"))?;
    }
    perform_operations(&operation, "创建设备项目")
}

fn file_operation() -> Result<IFileOperation, String> {
    file_operation_with_flags(FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_RENAMEONCOLLISION)
}

fn file_operation_with_flags(flags: FILEOPERATION_FLAGS) -> Result<IFileOperation, String> {
    let operation: IFileOperation = unsafe {
        CoCreateInstance(&FileOperation, None::<&windows::core::IUnknown>, CLSCTX_ALL)
            .map_err(|error| format!("无法创建 Windows 文件操作：{error}"))?
    };
    unsafe {
        operation
            .SetOperationFlags(flags)
            .map_err(|error| format!("无法配置 Windows 文件操作：{error}"))?;
    }
    Ok(operation)
}

fn perform_operations(operation: &IFileOperation, action: &str) -> Result<(), String> {
    unsafe {
        operation
            .PerformOperations()
            .map_err(|error| format!("无法{action}：{error}"))?;
        if operation
            .GetAnyOperationsAborted()
            .map_err(|error| format!("无法读取{action}结果：{error}"))?
            .as_bool()
        {
            return Err(format!("{action}已取消"));
        }
    }
    clear_shell_caches();
    Ok(())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn load_directory(path: &Path, sort: SortSpec) -> Result<DirectorySnapshot, String> {
    let _com = ComGuard::new()?;
    let folder = resolve_item(path).map_err(|error| format!("无法读取移动设备目录：{error}"))?;
    prime_shell_metadata(path, &folder);
    let enumerator: IEnumShellItems = unsafe {
        folder
            .BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)
            .map_err(|error| format!("无法枚举移动设备目录：{error}"))?
    };
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    loop {
        let mut batch: [Option<IShellItem>; 32] = std::array::from_fn(|_| None);
        let mut fetched = 0u32;
        unsafe {
            enumerator
                .Next(&mut batch, Some(&mut fetched))
                .map_err(|error| format!("读取移动设备项目失败：{error}"))?;
        }
        if fetched == 0 {
            break;
        }
        for item in batch.into_iter().take(fetched as usize).flatten() {
            match shell_entry(path, &item) {
                Some(entry) => entries.push(entry),
                None => skipped += 1,
            }
        }
    }
    sort_entries(&mut entries, sort);
    Ok(DirectorySnapshot {
        path: path.to_path_buf(),
        entries,
        skipped,
    })
}

pub(crate) fn parent_path(path: &Path) -> Option<PathBuf> {
    let _com = ComGuard::new().ok()?;
    let item = resolve_item(path).ok()?;
    let parent = unsafe { item.GetParent().ok()? };
    let value = display_name_for_item(&parent, SIGDN_DESKTOPABSOLUTEPARSING)?;
    (!value.is_empty()).then(|| PathBuf::from(value))
}

pub(crate) fn display_name(path: &Path) -> Option<String> {
    if let Some(name) = cached_display_name(path) {
        return Some(name);
    }
    let _com = ComGuard::new().ok()?;
    let item = resolve_item(path).ok()?;
    let name = display_name_for_item(&item, SIGDN_NORMALDISPLAY)?;
    cache_display_name(path, &name);
    Some(name)
}

pub(crate) fn is_directory(path: &Path) -> bool {
    let Ok(_com) = ComGuard::new() else {
        return false;
    };
    let Ok(item) = resolve_item(path) else {
        return false;
    };
    unsafe { item.GetAttributes(SFGAO_FOLDER) }
        .map(|attributes| (attributes.0 & SFGAO_FOLDER.0) != 0)
        .unwrap_or(false)
}

pub(crate) fn open_item(path: &Path) -> Result<(), String> {
    let operation = "open\0".encode_utf16().collect::<Vec<_>>();
    let file = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code <= 32 {
        Err(format!("无法打开移动设备项目：Windows 错误 {code}"))
    } else {
        Ok(())
    }
}

fn create_item(path: &Path) -> windows::core::Result<IShellItem> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) }
}

/// Resolves shell items whose absolute parsing name cannot be reparsed.
///
/// WPD children can expose a desktop-absolute parsing name that
/// SHCreateItemFromParsingName rejects. Walk down from a reparsable
/// ancestor and match the child through the Shell namespace enumerator.
fn resolve_item(path: &Path) -> Result<IShellItem, String> {
    let mut current_path = path.to_path_buf();
    let mut pending_names: Vec<OsString> = Vec::new();

    loop {
        let create_error = match create_item(&current_path) {
            Ok(mut item) => {
                while let Some(name) = pending_names.pop() {
                    item = find_child_item(&item, &name)?;
                }
                return Ok(item);
            }
            Err(error) => error,
        };

        let Some(name) = current_path.file_name() else {
            return Err(format!("无法解析移动设备路径：{create_error}"));
        };
        pending_names.push(name.to_os_string());

        let Some(parent) = current_path.parent() else {
            return Err(format!("无法定位移动设备项目：{}", path.display()));
        };
        if parent == current_path {
            return Err(format!("无法定位移动设备项目：{}", path.display()));
        }
        current_path = parent.to_path_buf();
    }
}

fn find_child_item(parent: &IShellItem, relative_name: &OsStr) -> Result<IShellItem, String> {
    let direct_name = relative_name
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if let Ok(item) = unsafe {
        SHCreateItemFromRelativeName::<_, _, _, IShellItem>(
            parent,
            PCWSTR(direct_name.as_ptr()),
            None::<&IBindCtx>,
        )
    } {
        return Ok(item);
    }

    let expected = relative_name.to_string_lossy();
    let enumerator: IEnumShellItems = unsafe {
        parent
            .BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)
            .map_err(|error| format!("无法枚举移动设备父目录：{error}"))?
    };

    loop {
        let mut batch: [Option<IShellItem>; 32] = std::array::from_fn(|_| None);
        let mut fetched = 0u32;
        unsafe {
            enumerator
                .Next(&mut batch, Some(&mut fetched))
                .map_err(|error| format!("读取移动设备子项目失败：{error}"))?;
        }
        if fetched == 0 {
            break;
        }
        for item in batch.into_iter().take(fetched as usize).flatten() {
            let matches_parent_relative = display_name_for_item(&item, SIGDN_PARENTRELATIVEPARSING)
                .is_some_and(|value| value.eq_ignore_ascii_case(&expected));
            let matches_display_name = display_name_for_item(&item, SIGDN_NORMALDISPLAY)
                .is_some_and(|value| value.eq_ignore_ascii_case(&expected));
            let matches_desktop_absolute =
                display_name_for_item(&item, SIGDN_DESKTOPABSOLUTEPARSING).is_some_and(|value| {
                    Path::new(&value)
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(&expected))
                });
            if matches_parent_relative || matches_display_name || matches_desktop_absolute {
                return Ok(item);
            }
        }
    }

    Err(format!("无法定位移动设备项目：{expected}"))
}
fn relative_child_path(parent: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let relative = Path::new(relative);
    let mut saw_component = false;
    for component in relative.components() {
        match component {
            Component::Normal(_) => saw_component = true,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    saw_component.then(|| parent.join(relative))
}

fn shell_path_for_item(parent: &Path, item: &IShellItem) -> Option<PathBuf> {
    if let Some(path) = display_name_for_item(item, SIGDN_PARENTRELATIVEPARSING)
        .and_then(|relative| relative_child_path(parent, &relative))
    {
        return Some(path);
    }
    display_name_for_item(item, SIGDN_DESKTOPABSOLUTEPARSING)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn shell_entry(parent: &Path, item: &IShellItem) -> Option<FileEntry> {
    let name =
        display_name_for_item(item, SIGDN_NORMALDISPLAY).filter(|value| !value.is_empty())?;
    let path = shell_path_for_item(parent, item)?;
    let is_dir = unsafe { item.GetAttributes(SFGAO_FOLDER) }
        .map(|attributes| (attributes.0 & SFGAO_FOLDER.0) != 0)
        .unwrap_or(false);
    let extension = Path::new(&name)
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Some(FileEntry {
        path,
        name,
        kind: if is_dir {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: 0,
        modified: None,
        extension,
        git_status: None,
        svn_status: None,
    })
}

fn display_name_for_item(
    item: &IShellItem,
    kind: windows::Win32::UI::Shell::SIGDN,
) -> Option<String> {
    let value = unsafe { item.GetDisplayName(kind).ok()? };
    Some(take_pwstr(value))
}

fn take_pwstr(value: PWSTR) -> String {
    if value.0.is_null() {
        return String::new();
    }
    let mut length = 0usize;
    while unsafe { *value.0.add(length) } != 0 {
        length += 1;
    }
    let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value.0, length) });
    unsafe { CoTaskMemFree(Some(value.0.cast::<c_void>())) };
    result
}

struct ComGuard(bool);

impl ComGuard {
    fn new() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_err() && result != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Err(format!("无法初始化移动设备访问：{result:?}"));
        }
        Ok(Self(result.is_ok()))
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cached_item_capabilities_or_else, clear_shell_caches, copy_item, is_shell_namespace_path,
        relative_child_path,
    };
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn copy_test_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kite-shell-copy-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create test root");
        root
    }

    #[test]
    fn copy_with_progress_preserves_existing_folder_and_copies_contents() {
        let root = copy_test_root("collision");
        let source = root.join("source");
        let destination = root.join("destination");
        let existing = destination.join("source");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(&existing).unwrap();
        fs::write(source.join("note.txt"), b"new").unwrap();
        fs::write(existing.join("note.txt"), b"original").unwrap();
        let expected = crate::fs::unique_destination(&destination, source.file_name().unwrap());

        copy_item(&source, &destination, None).expect("copy the folder through the Shell");

        assert_eq!(fs::read(expected.join("note.txt")).unwrap(), b"new");
        assert_eq!(fs::read(existing.join("note.txt")).unwrap(), b"original");
        assert_eq!(fs::read(source.join("note.txt")).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_with_progress_rejects_a_destination_inside_source() {
        let source = copy_test_root("recursive");
        let destination = source.join("inside");
        fs::create_dir(&destination).unwrap();

        let error = copy_item(&source, &destination, None).unwrap_err();

        assert_eq!(error, "不能把文件夹复制或移动到自身内部");
        assert!(fs::read_dir(&destination).unwrap().next().is_none());
        fs::remove_dir_all(source).unwrap();
    }

    #[test]
    #[ignore = "requires a connected portable device"]
    fn live_portable_device_directory_loads() {
        let device = crate::devices::snapshot()
            .portable_devices
            .into_iter()
            .next()
            .expect("portable device");
        let root_snapshot = super::load_directory(
            Path::new(&device.shell_path),
            crate::fs::SortSpec::default(),
        )
        .expect("load portable device");
        assert!(!root_snapshot.entries.is_empty());

        let child = root_snapshot
            .entries
            .iter()
            .find(|entry| entry.is_dir())
            .expect("portable device directory");
        let child_snapshot = super::load_directory(&child.path, crate::fs::SortSpec::default())
            .unwrap_or_else(|error| {
                panic!("load portable directory {}: {error}", child.path.display())
            });
        assert!(!child_snapshot.entries.is_empty());
    }

    #[test]
    fn recognizes_shell_namespace_paths() {
        assert!(is_shell_namespace_path(Path::new(
            r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\\?\usb#vid_05ac"
        )));
        assert!(is_shell_namespace_path(Path::new("shell:MyComputerFolder")));
        assert!(!is_shell_namespace_path(Path::new(r"C:\Users")));
    }

    #[test]
    fn joins_parent_relative_names_to_the_current_shell_directory() {
        let parent =
            Path::new(r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\\?\usb#vid_2d95&pid_6013");
        let expected = parent.join(r"SID-{10001,,487650308096}");
        assert_eq!(
            relative_child_path(parent, r"SID-{10001,,487650308096}"),
            Some(expected)
        );
    }

    #[test]
    fn rejects_absolute_and_parent_relative_shell_names() {
        let parent = Path::new(r"::{20D04FE0-3AEA-1069-A2D8-08002B30309D}");
        assert_eq!(relative_child_path(parent, ""), None);
        assert_eq!(relative_child_path(parent, "."), None);
        assert_eq!(relative_child_path(parent, r"C:\Windows"), None);
        assert_eq!(relative_child_path(parent, r"\Windows"), None);
        assert_eq!(relative_child_path(parent, r"..\Windows"), None);
    }

    #[test]
    fn caches_item_capabilities_after_the_first_lookup() {
        clear_shell_caches();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = PathBuf::from(format!(
            "kite-test-shell-capabilities-{}-{unique}",
            std::process::id()
        ));
        let calls = Cell::new(0);

        let first = cached_item_capabilities_or_else(&path, || {
            calls.set(calls.get() + 1);
            Some((true, false, true))
        });
        let second = cached_item_capabilities_or_else(&path, || {
            calls.set(calls.get() + 1);
            Some((false, true, false))
        });

        assert_eq!(first, (true, false, true));
        assert_eq!(second, first);
        assert_eq!(calls.get(), 1);
        clear_shell_caches();
    }
}
