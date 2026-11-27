use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, c_void};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use kite_core::context_menu_model::{OpenWithApplication, OpenWithCatalog, OpenWithCatalogStatus};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject};
use windows::Win32::UI::Shell::{
    ASSOC_FILTER_NONE, ASSOCF_NONE, ASSOCSTR_EXECUTABLE, ASSOCSTR_FRIENDLYAPPNAME,
    AssocQueryStringW, BHID_DataObject, IAssocHandler, IShellItem, SHAssocEnumHandlers,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, SHGetIDListFromObject,
};
use windows::core::PCWSTR;

const MRU_LIMIT: usize = 5;
const MRU_FILE: &str = "open-with-mru.json";

#[derive(Clone, Debug)]
struct HandlerRecord {
    key: String,
    label: String,
}

pub(crate) fn catalog(path: &Path) -> OpenWithCatalog {
    if !is_regular_readable_file(path) {
        return OpenWithCatalog::default();
    }

    let handlers = enumerate_handlers(path).unwrap_or_default();
    let default_executable = assoc_query(path, ASSOCSTR_EXECUTABLE);
    let default_friendly_name = assoc_query(path, ASSOCSTR_FRIENDLYAPPNAME);
    let default_key = default_executable
        .as_deref()
        .map(canonical_handler_key)
        .filter(|key| !key.is_empty());
    let handler_map = handlers
        .iter()
        .map(|handler| (handler.key.clone(), handler.clone()))
        .collect::<HashMap<_, _>>();
    let default_application_key = default_key.as_ref().map(|key| {
        if handler_map.contains_key(key) {
            key.clone()
        } else {
            format!("default:{key}")
        }
    });

    let current_keys = handlers
        .iter()
        .map(|handler| handler.key.as_str())
        .chain(default_application_key.as_deref())
        .collect::<HashSet<_>>();
    let mru = sanitized_mru(&current_keys);
    if mru != load_mru() {
        let _ = save_mru(&mru);
    }

    let default_application = default_application_key.map(|application_key| {
        let display_name = default_friendly_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                default_key
                    .as_ref()
                    .and_then(|key| handler_map.get(key))
                    .map(|handler| handler.label.clone())
            })
            .or_else(|| {
                default_executable
                    .as_deref()
                    .and_then(executable_display_name)
            })
            .unwrap_or_else(|| "默认应用".to_string());
        OpenWithApplication {
            application_id: application_key,
            display_name,
            is_default: true,
            is_recent: false,
            is_available: true,
            unavailable_reason: None,
        }
    });

    let mut applications = Vec::new();
    let mut seen = HashSet::new();
    if let Some(default_application) = default_application {
        seen.insert(default_application.application_id.clone());
        applications.push(default_application);
    }
    for key in &mru {
        if seen.insert(key.clone())
            && let Some(handler) = handler_map.get(key)
        {
            applications.push(application_from_handler(handler, true));
        }
    }
    for handler in &handlers {
        if seen.insert(handler.key.clone()) {
            applications.push(application_from_handler(handler, false));
        }
    }

    OpenWithCatalog {
        status: if applications.is_empty() {
            OpenWithCatalogStatus::Unavailable
        } else {
            OpenWithCatalogStatus::Ready
        },
        applications,
    }
}

pub(crate) fn launch(path: &Path, application_id: &str) -> Result<(), String> {
    if !is_regular_readable_file(path) {
        return Err("只能对存在且可读的普通文件使用“打开方式”".to_string());
    }
    let requested_key = canonical_handler_key(application_id);
    if requested_key.is_empty() {
        return Err("打开方式应用标识为空".to_string());
    }

    if requested_key.starts_with("default:") {
        crate::fs::open_file(path)?;
        remember_application(&requested_key);
        return Ok(());
    }

    let handlers = enumerate_handlers(path)?;
    let handler = handlers
        .into_iter()
        .find(|handler| handler.key == requested_key)
        .ok_or_else(|| "所选应用已卸载或不再处理此文件类型，请重新选择应用".to_string())?;
    let handler = handler_interface(path, &handler.key)?;
    let data_object = data_object_for_path(path)?;
    unsafe { handler.Invoke(&data_object) }
        .map_err(|error| format!("无法使用所选应用打开文件：{error}"))?;
    remember_application(&requested_key);
    Ok(())
}

fn application_from_handler(handler: &HandlerRecord, is_recent: bool) -> OpenWithApplication {
    OpenWithApplication {
        application_id: handler.key.clone(),
        display_name: handler.label.clone(),
        is_default: false,
        is_recent,
        is_available: true,
        unavailable_reason: None,
    }
}

fn enumerate_handlers(path: &Path) -> Result<Vec<HandlerRecord>, String> {
    let extension = path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .filter(|extension| extension.len() > 1)
        .ok_or_else(|| "文件没有扩展名，无法枚举兼容应用".to_string())?;
    let extension = wide_null(&extension);
    let enumerator = unsafe { SHAssocEnumHandlers(PCWSTR(extension.as_ptr()), ASSOC_FILTER_NONE) }
        .map_err(|error| format!("无法枚举文件的兼容应用：{error}"))?;

    let mut records = Vec::new();
    let mut seen = HashSet::new();
    loop {
        let mut handlers = std::array::from_fn::<_, 16, _>(|_| None::<IAssocHandler>);
        let mut fetched = 0u32;
        unsafe { enumerator.Next(&mut handlers, Some(&mut fetched)) }
            .map_err(|error| format!("读取兼容应用失败：{error}"))?;
        if fetched == 0 {
            break;
        }
        for handler in handlers.into_iter().take(fetched as usize).flatten() {
            let Some((key, label)) = handler_identity(&handler) else {
                continue;
            };
            if seen.insert(key.clone()) {
                records.push(HandlerRecord { key, label });
            }
        }
    }
    Ok(records)
}

fn handler_interface(path: &Path, expected_key: &str) -> Result<IAssocHandler, String> {
    let extension = path
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .ok_or_else(|| "文件没有扩展名，无法重新确认应用".to_string())?;
    let extension = wide_null(&extension);
    let enumerator = unsafe { SHAssocEnumHandlers(PCWSTR(extension.as_ptr()), ASSOC_FILTER_NONE) }
        .map_err(|error| format!("无法重新枚举兼容应用：{error}"))?;
    loop {
        let mut handlers = std::array::from_fn::<_, 16, _>(|_| None::<IAssocHandler>);
        let mut fetched = 0u32;
        unsafe { enumerator.Next(&mut handlers, Some(&mut fetched)) }
            .map_err(|error| format!("读取兼容应用失败：{error}"))?;
        if fetched == 0 {
            break;
        }
        for handler in handlers.into_iter().take(fetched as usize).flatten() {
            if handler_identity(&handler).is_some_and(|(key, _)| key == expected_key) {
                return Ok(handler);
            }
        }
    }
    Err("所选应用已卸载或不再处理此文件类型，请重新选择应用".to_string())
}

fn handler_identity(handler: &IAssocHandler) -> Option<(String, String)> {
    let name = unsafe { handler.GetName().ok() }?;
    let raw_name = unsafe { name.to_string().ok() };
    unsafe { CoTaskMemFree(Some(name.0.cast::<c_void>())) };
    let key = canonical_handler_key(raw_name.as_deref()?);
    if key.is_empty() {
        return None;
    }

    let ui_name = unsafe { handler.GetUIName().ok() };
    let label = ui_name
        .as_ref()
        .and_then(|name| unsafe { name.to_string().ok() })
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .or_else(|| executable_display_name(raw_name.as_deref()?))
        .unwrap_or_else(|| "应用".to_string());
    if let Some(ui_name) = ui_name {
        unsafe { CoTaskMemFree(Some(ui_name.0.cast::<c_void>())) };
    }
    Some((key, label))
}

fn data_object_for_path(path: &Path) -> Result<IDataObject, String> {
    let path = wide_os_null(path.as_os_str());
    let item: IShellItem = unsafe {
        SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None::<&IBindCtx>)
    }
    .map_err(|error| format!("无法读取文件 Shell 项目：{error}"))?;
    let pidl = unsafe { SHGetIDListFromObject(&item) }
        .map_err(|error| format!("无法读取文件 Shell 项目 ID：{error}"))?;
    let items_result = unsafe { SHCreateShellItemArrayFromIDLists(&[pidl as *const _]) };
    unsafe { CoTaskMemFree(Some(pidl as *const c_void)) };
    let items = items_result.map_err(|error| format!("无法创建文件 Shell 项目数组：{error}"))?;
    unsafe { items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
        .map_err(|error| format!("无法创建打开方式数据对象：{error}"))
}

fn assoc_query(path: &Path, association: windows::Win32::UI::Shell::ASSOCSTR) -> Option<String> {
    let association_target = if path.extension().is_some() {
        format!(".{}", path.extension()?.to_string_lossy())
    } else {
        path.to_string_lossy().into_owned()
    };
    let association_target = wide_null(&association_target);
    let mut length = 0u32;
    let _ = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            association,
            PCWSTR(association_target.as_ptr()),
            PCWSTR::null(),
            None,
            &mut length,
        )
    };
    if length == 0 {
        return None;
    }
    let mut buffer = vec![0u16; length as usize];
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            association,
            PCWSTR(association_target.as_ptr()),
            PCWSTR::null(),
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut length,
        )
    };
    if !result.is_ok() || length == 0 {
        return None;
    }
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(length as usize);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn canonical_handler_key(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let path = Path::new(value);
    if path.is_absolute() {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        canonical
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
    } else {
        value.to_ascii_lowercase()
    }
}

fn executable_display_name(value: &str) -> Option<String> {
    Path::new(value)
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn is_regular_readable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
        && std::fs::File::open(path).is_ok()
}

fn remember_application(application_id: &str) {
    let mut mru = load_mru();
    mru.retain(|existing| existing != application_id);
    mru.insert(0, application_id.to_string());
    mru.truncate(MRU_LIMIT);
    let _ = save_mru(&mru);
}

fn sanitized_mru(current_keys: &HashSet<&str>) -> Vec<String> {
    let mut mru = load_mru();
    mru.retain(|key| current_keys.contains(key.as_str()));
    let mut seen = HashSet::new();
    mru.retain(|key| seen.insert(key.clone()));
    mru.truncate(MRU_LIMIT);
    mru
}

fn load_mru() -> Vec<String> {
    let Some(path) = mru_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<String>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .map(|key| canonical_handler_key(&key))
        .filter(|key| !key.is_empty())
        .collect()
}

fn save_mru(mru: &[String]) -> Result<(), String> {
    let path = mru_path().ok_or_else(|| "无法定位 Kite 配置目录".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建 Kite 配置目录：{error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(mru)
        .map_err(|error| format!("无法序列化打开方式记录：{error}"))?;
    atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
        .write(|file| file.write_all(&bytes))
        .map_err(|error| format!("无法保存打开方式记录：{error}"))
}

fn mru_path() -> Option<PathBuf> {
    dirs::config_dir().map(|directory| directory.join("Kite").join(MRU_FILE))
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_os_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
