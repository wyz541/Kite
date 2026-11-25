use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use walkdir::WalkDir;
use zip::read::ZipFile;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
}

impl ArchiveFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "zip" => Some(Self::Zip),
            "tar.gz" | "tgz" => Some(Self::TarGz),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar.gz",
        }
    }
}

pub fn format_for_path(path: &Path) -> Option<ArchiveFormat> {
    archive_name_and_format(&path.file_name()?.to_string_lossy()).map(|(_, format)| format)
}

fn archive_name_and_format(name: &str) -> Option<(&str, ArchiveFormat)> {
    [
        (".zip", ArchiveFormat::Zip),
        (".tar.gz", ArchiveFormat::TarGz),
        (".tgz", ArchiveFormat::TarGz),
    ]
    .into_iter()
    .find_map(|(suffix, format)| {
        let stem_end = name.len().checked_sub(suffix.len())?;
        let (stem, extension) = name.split_at_checked(stem_end)?;
        extension
            .eq_ignore_ascii_case(suffix)
            .then_some((stem, format))
    })
}

pub fn is_supported_archive(path: &Path) -> bool {
    format_for_path(path).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchivePhase {
    Checking,
    Working,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveProgress<'a> {
    Destination(&'a Path),
    Bytes {
        phase: ArchivePhase,
        processed: u64,
        total: Option<u64>,
    },
    Finishing,
}

#[derive(Default)]
struct ProgressReporter<'a> {
    callback: Option<&'a mut dyn FnMut(ArchiveProgress<'_>)>,
    last_update: Option<Instant>,
}

impl<'a> ProgressReporter<'a> {
    fn new(callback: &'a mut dyn FnMut(ArchiveProgress<'_>)) -> Self {
        Self {
            callback: Some(callback),
            last_update: None,
        }
    }

    fn emit(&mut self, event: ArchiveProgress<'_>, force: bool) {
        if let Some(callback) = self.callback.as_mut()
            && (force
                || self
                    .last_update
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(100)))
        {
            self.last_update = Some(Instant::now());
            callback(event);
        }
    }

    fn bytes(&mut self, phase: ArchivePhase, processed: u64, total: Option<u64>, force: bool) {
        self.emit(
            ArchiveProgress::Bytes {
                phase,
                processed,
                total,
            },
            force,
        );
    }

    fn meter<T>(
        &mut self,
        stream: T,
        phase: ArchivePhase,
        processed: u64,
        total: Option<u64>,
    ) -> Meter<'_, 'a, T> {
        Meter {
            stream,
            report: self,
            phase,
            processed,
            total,
        }
    }
}

struct Meter<'r, 'a, T> {
    stream: T,
    report: &'r mut ProgressReporter<'a>,
    phase: ArchivePhase,
    processed: u64,
    total: Option<u64>,
}

impl<T> Meter<'_, '_, T> {
    fn advance(&mut self, bytes: usize) {
        self.processed = self.processed.saturating_add(bytes as u64);
        self.report
            .bytes(self.phase, self.processed, self.total, false);
    }

    fn finish(self) -> T {
        self.report
            .bytes(self.phase, self.processed, self.total, true);
        self.stream
    }
}

impl<T: Read> Read for Meter<'_, '_, T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.stream.read(buffer)?;
        self.advance(read);
        Ok(read)
    }
}

impl<T: Write> Write for Meter<'_, '_, T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.stream.write(buffer)?;
        self.advance(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

pub fn compress(source: &Path, format: ArchiveFormat) -> Result<PathBuf, String> {
    compress_impl(source, format, &mut ProgressReporter::default())
}

pub fn compress_with_progress(
    source: &Path,
    format: ArchiveFormat,
    mut progress: impl FnMut(ArchiveProgress<'_>),
) -> Result<PathBuf, String> {
    compress_impl(source, format, &mut ProgressReporter::new(&mut progress))
}

fn compress_impl(
    source: &Path,
    format: ArchiveFormat,
    report: &mut ProgressReporter<'_>,
) -> Result<PathBuf, String> {
    let metadata = fs::metadata(source).map_err(|error| format!("无法读取待压缩项目：{error}"))?;
    let parent = source
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "不能压缩根目录".to_string())?;
    let original_name = source
        .file_name()
        .ok_or_else(|| "无法确定待压缩项目名称".to_string())?;
    let source_name = if metadata.is_dir() {
        original_name
    } else {
        source.file_stem().unwrap_or(original_name)
    };
    let total = validate_compressible_source(
        source,
        format == ArchiveFormat::Zip && report.callback.is_some(),
    )?;
    let destination = unique_archive_path(parent, source_name, format);
    let temporary = temporary_archive_path(parent, source_name, format);
    report.emit(ArchiveProgress::Destination(&destination), true);

    let result = match format {
        ArchiveFormat::Zip => write_zip(source, &temporary, total, report),
        ArchiveFormat::TarGz => write_tar_gz(source, &temporary, metadata.is_dir(), report),
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if let Err(error) = fs::rename(&temporary, &destination) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法保存压缩文件：{error}"));
    }
    Ok(destination)
}

pub fn extract_here(source: &Path) -> Result<PathBuf, String> {
    extract_here_impl(source, &mut ProgressReporter::default())
}

pub fn extract_here_with_progress(
    source: &Path,
    mut progress: impl FnMut(ArchiveProgress<'_>),
) -> Result<PathBuf, String> {
    extract_here_impl(source, &mut ProgressReporter::new(&mut progress))
}

fn extract_here_impl(source: &Path, report: &mut ProgressReporter<'_>) -> Result<PathBuf, String> {
    let file_name = source
        .file_name()
        .ok_or_else(|| "无法确定压缩文件名称".to_string())?
        .to_string_lossy();
    let (name, format) = archive_name_and_format(&file_name)
        .ok_or_else(|| "不支持此压缩格式。当前支持 ZIP 和 TAR.GZ。".to_string())?;
    let parent = source
        .parent()
        .ok_or_else(|| "无法确定解压目标文件夹".to_string())?;
    let name = if cfg!(windows) {
        // Windows normalizes trailing dots and spaces in directory names.
        name.trim_end_matches(['.', ' '])
    } else {
        name
    };
    let name = match Path::new(name).components().next() {
        Some(Component::Normal(_)) => name,
        _ => "解压文件",
    };

    let mut index = 0;
    let destination = loop {
        let path = if index == 0 {
            parent.join(name)
        } else {
            parent.join(format!("{name} ({index})"))
        };
        match fs::create_dir(&path) {
            Ok(()) => break path,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => index += 1,
            Err(error) => return Err(format!("无法创建解压目标文件夹：{error}")),
        }
    };

    if let Err(error) = extract_contents(source, &destination, format, report) {
        let _ = fs::remove_dir(&destination);
        return Err(error);
    }
    Ok(destination)
}

pub fn extract(source: &Path, destination: &Path) -> Result<PathBuf, String> {
    extract_impl(source, destination, &mut ProgressReporter::default())
}

pub fn extract_with_progress(
    source: &Path,
    destination: &Path,
    mut progress: impl FnMut(ArchiveProgress<'_>),
) -> Result<PathBuf, String> {
    extract_impl(
        source,
        destination,
        &mut ProgressReporter::new(&mut progress),
    )
}

fn extract_impl(
    source: &Path,
    destination: &Path,
    report: &mut ProgressReporter<'_>,
) -> Result<PathBuf, String> {
    let format = format_for_path(source)
        .ok_or_else(|| "不支持此压缩格式。当前支持 ZIP 和 TAR.GZ。".to_string())?;
    fs::create_dir_all(destination).map_err(|error| format!("无法创建解压目标文件夹：{error}"))?;

    extract_contents(source, destination, format, report)?;
    Ok(destination.to_path_buf())
}

fn extract_contents(
    source: &Path,
    destination: &Path,
    format: ArchiveFormat,
    report: &mut ProgressReporter<'_>,
) -> Result<(), String> {
    report.emit(ArchiveProgress::Destination(destination), true);
    match format {
        ArchiveFormat::Zip => extract_zip(source, destination, report),
        ArchiveFormat::TarGz => extract_tar_gz(source, destination, report),
    }
}

fn validate_compressible_source(source: &Path, measure: bool) -> Result<Option<u64>, String> {
    let mut total = measure.then_some(0u64);
    for item in WalkDir::new(source).follow_links(false) {
        let entry = item.map_err(|error| format!("读取待压缩项目失败：{error}"))?;
        if entry.path_is_symlink() {
            return Err(format!("暂不支持压缩符号链接：{}", entry.path().display()));
        }
        if let Some(bytes) = total.as_mut()
            && entry.file_type().is_file()
        {
            let metadata = entry
                .metadata()
                .map_err(|error| format!("读取待压缩项目属性失败：{error}"))?;
            *bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn write_zip(
    source: &Path,
    destination: &Path,
    total: Option<u64>,
    report: &mut ProgressReporter<'_>,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("无法创建 ZIP 文件：{error}"))?;
    report.bytes(ArchivePhase::Working, 0, total, true);
    let mut writer = report.meter(ZipWriter::new(file), ArchivePhase::Working, 0, total);
    let parent = source
        .parent()
        .ok_or_else(|| "不能压缩根目录".to_string())?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o755);

    for item in WalkDir::new(source).follow_links(false) {
        let entry = item.map_err(|error| format!("读取待压缩项目失败：{error}"))?;
        if entry.path_is_symlink() {
            return Err(format!("暂不支持压缩符号链接：{}", entry.path().display()));
        }
        let relative = entry
            .path()
            .strip_prefix(parent)
            .map_err(|_| "无法生成压缩文件内部路径".to_string())?;
        let name = zip_entry_name(relative)?;
        if name.is_empty() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("读取待压缩项目属性失败：{error}"))?;
        if metadata.is_dir() {
            writer
                .stream
                .add_directory_from_path(Path::new(&name), options)
                .map_err(|error| format!("写入 ZIP 文件夹失败：{error}"))?;
        } else if metadata.is_file() {
            writer
                .stream
                .start_file_from_path(
                    Path::new(&name),
                    options.large_file(metadata.len() >= u32::MAX as u64),
                )
                .map_err(|error| format!("写入 ZIP 文件项失败：{error}"))?;
            let mut input =
                File::open(entry.path()).map_err(|error| format!("无法读取待压缩文件：{error}"))?;
            io::copy(&mut input, &mut writer)
                .map_err(|error| format!("写入 ZIP 文件内容失败：{error}"))?;
        } else {
            return Err(format!("暂不支持压缩特殊文件：{}", entry.path().display()));
        }
    }

    let writer = writer.finish();
    report.emit(ArchiveProgress::Finishing, true);
    writer
        .finish()
        .map_err(|error| format!("完成 ZIP 文件失败：{error}"))?;
    Ok(())
}

fn write_tar_gz(
    source: &Path,
    destination: &Path,
    directory: bool,
    report: &mut ProgressReporter<'_>,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| format!("无法创建 TAR.GZ 文件：{error}"))?;
    let encoder = GzEncoder::new(file, Compression::default());
    report.bytes(ArchivePhase::Working, 0, None, true);
    let mut meter = report.meter(encoder, ArchivePhase::Working, 0, None);
    let mut builder = tar::Builder::new(&mut meter);
    let parent = source
        .parent()
        .ok_or_else(|| "不能压缩根目录".to_string())?;
    let relative = source
        .strip_prefix(parent)
        .map_err(|_| "无法生成压缩文件内部路径".to_string())?;
    builder.follow_symlinks(false);
    if directory {
        builder.append_dir_all(relative, source)
    } else {
        builder.append_path_with_name(source, relative)
    }
    .map_err(|error| format!("写入 TAR.GZ 文件失败：{error}"))?;
    builder
        .into_inner()
        .map_err(|error| format!("完成 TAR.GZ 文件失败：{error}"))?;
    let encoder = meter.finish();
    report.emit(ArchiveProgress::Finishing, true);
    encoder
        .finish()
        .map_err(|error| format!("完成 TAR.GZ 压缩失败：{error}"))?;
    Ok(())
}

fn extract_zip(
    source: &Path,
    destination: &Path,
    report: &mut ProgressReporter<'_>,
) -> Result<(), String> {
    let file = File::open(source).map_err(|error| format!("无法打开 ZIP 文件：{error}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("无法读取 ZIP 文件：{error}"))?;
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut total = 0u64;

    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("无法读取 ZIP 文件项：{error}"))?;
        let Some(relative) = zip_entry_path(&entry) else {
            return Err(format!("ZIP 文件包含不安全路径：{}", entry.name()));
        };
        validate_relative_path(&relative)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(relative.clone()) {
            return Err(format!("ZIP 文件包含重复路径：{}", relative.display()));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("暂不支持解压包含符号链接的 ZIP 文件".to_string());
        }
        let target = destination.join(&relative);
        if entry.is_dir() {
            directories.push(target);
        } else {
            ensure_available(&target)?;
            total = total.saturating_add(entry.size());
            files.push((index, target));
        }
    }

    let total = Some(total);
    let mut processed = 0;
    report.bytes(ArchivePhase::Working, processed, total, true);
    for directory in directories {
        fs::create_dir_all(&directory).map_err(|error| format!("创建 ZIP 文件夹失败：{error}"))?;
    }
    for (index, target) in files {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建 ZIP 文件夹失败：{error}"))?;
        }
        let mut input = archive
            .by_index(index)
            .map_err(|error| format!("无法读取 ZIP 文件项：{error}"))?;
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("无法创建解压文件 {}：{error}", target.display()))?;
        let mut meter = report.meter(output, ArchivePhase::Working, processed, total);
        io::copy(&mut input, &mut meter).map_err(|error| format!("写入解压文件失败：{error}"))?;
        processed = meter.processed;
    }
    report.bytes(ArchivePhase::Working, processed, total, true);
    Ok(())
}

fn zip_entry_path(entry: &ZipFile<'_>) -> Option<PathBuf> {
    // ZIP specifies CP437 when the UTF-8 flag is absent, but legacy Windows
    // archives commonly store GBK bytes. Only override the crate's decoded
    // name when that result is visibly CP437 mojibake and GBK recovers CJK.
    let current = entry.enclosed_name()?;
    let raw = entry.name_raw();
    if raw.is_ascii() {
        return Some(current);
    }

    if let Ok(decoded) = std::str::from_utf8(raw) {
        return Some(PathBuf::from(decoded));
    }

    let (decoded, had_errors) = encoding_rs::GBK.decode_without_bom_handling(raw);
    if had_errors
        || !decoded.chars().any(is_cjk)
        || current.to_string_lossy().chars().any(is_cjk)
        || !looks_like_legacy_mojibake(&current.to_string_lossy())
    {
        return Some(current);
    }

    Some(PathBuf::from(decoded.as_ref()))
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
            | 0x30000..=0x323AF
    )
}

fn looks_like_legacy_mojibake(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character as u32,
            0x0370..=0x03FF
                | 0x2000..=0x2BFF
                | 0x3000..=0x303F
                | 0x3200..=0x33FF
        )
    })
}

fn extract_tar_gz(
    source: &Path,
    destination: &Path,
    report: &mut ProgressReporter<'_>,
) -> Result<(), String> {
    let file = File::open(source).map_err(|error| format!("无法打开 TAR.GZ 文件：{error}"))?;
    let total = file.metadata().ok().map(|metadata| metadata.len());
    report.bytes(ArchivePhase::Checking, 0, total, true);
    let mut meter = report.meter(file, ArchivePhase::Checking, 0, total);
    validate_tar_gz(GzDecoder::new(&mut meter), destination)?;
    meter.finish();

    let file = File::open(source).map_err(|error| format!("无法打开 TAR.GZ 文件：{error}"))?;
    report.bytes(ArchivePhase::Working, 0, total, true);
    let mut meter = report.meter(file, ArchivePhase::Working, 0, total);
    let decoder = GzDecoder::new(&mut meter);
    let mut archive = tar::Archive::new(decoder);
    for item in archive
        .entries()
        .map_err(|error| format!("无法读取 TAR.GZ 文件：{error}"))?
    {
        let mut entry = item.map_err(|error| format!("无法读取 TAR.GZ 文件项：{error}"))?;
        let unpacked = entry
            .unpack_in(destination)
            .map_err(|error| format!("解压 TAR.GZ 文件项失败：{error}"))?;
        if !unpacked {
            return Err("TAR.GZ 文件包含不安全路径".to_string());
        }
    }
    meter.finish();
    Ok(())
}

fn validate_tar_gz(decoder: impl Read, destination: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(decoder);
    let mut seen = BTreeSet::new();

    for item in archive
        .entries()
        .map_err(|error| format!("无法读取 TAR.GZ 文件：{error}"))?
    {
        let entry = item.map_err(|error| format!("无法读取 TAR.GZ 文件项：{error}"))?;
        let relative = entry
            .path()
            .map_err(|error| format!("TAR.GZ 文件路径无效：{error}"))?
            .to_path_buf();
        validate_relative_path(&relative)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !seen.insert(relative.clone()) {
            return Err(format!("TAR.GZ 文件包含重复路径：{}", relative.display()));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err("暂不支持解压包含符号链接的 TAR.GZ 文件".to_string());
        }
        if !entry_type.is_dir() && !entry_type.is_file() {
            return Err(format!(
                "TAR.GZ 文件包含不支持的项目：{}",
                relative.display()
            ));
        }
        if entry_type.is_file() {
            ensure_available(&destination.join(relative))?;
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        return Err(format!("压缩文件包含绝对路径：{}", path.display()));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("压缩文件包含不安全路径：{}", path.display()));
            }
        }
    }
    Ok(())
}

fn zip_entry_name(path: &Path) -> Result<String, String> {
    validate_relative_path(path)?;
    let parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(parts.join("/"))
}

fn ensure_available(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        Err(format!("目标文件已经存在：{}", path.display()))
    } else {
        Ok(())
    }
}

fn unique_archive_path(
    parent: &Path,
    source_name: &std::ffi::OsStr,
    format: ArchiveFormat,
) -> PathBuf {
    let base = source_name.to_string_lossy();
    for index in 0.. {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!(" ({index})")
        };
        let candidate = parent.join(format!("{base}{suffix}.{}", format.extension()));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    unreachable!("archive path counter is unbounded")
}

fn temporary_archive_path(
    parent: &Path,
    source_name: &std::ffi::OsStr,
    format: ArchiveFormat,
) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".{}.kite-{}-{nonce}.{}",
        source_name.to_string_lossy(),
        std::process::id(),
        format.extension()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "kite-archive-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn fixture(label: &str) -> (PathBuf, PathBuf) {
        let root = temporary_root(label);
        let source = root.join("sample");
        fs::create_dir_all(source.join("nested")).expect("create fixture");
        fs::write(source.join("hello.txt"), b"hello").expect("write fixture");
        fs::write(source.join("nested").join("data.txt"), b"data").expect("write nested fixture");
        (root, source)
    }

    #[test]
    fn detects_supported_archive_names() {
        assert_eq!(
            format_for_path(Path::new("report.ZIP")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            format_for_path(Path::new("report.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            format_for_path(Path::new("report.tgz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(format_for_path(Path::new("report.7z")), None);
    }

    #[test]
    fn file_archive_name_uses_the_file_stem() {
        let (root, source) = fixture("name");
        let file = source.join("hello.txt");
        let archive = compress(&file, ArchiveFormat::Zip).expect("compress");
        assert_eq!(
            archive.file_name().and_then(|name| name.to_str()),
            Some("hello.zip")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extract_here_puts_loose_files_in_a_named_folder() {
        let (root, source) = fixture("extract-here-zip");
        let archive = compress(&source.join("hello.txt"), ArchiveFormat::Zip).expect("compress");
        let archive_path = root.join("report v1.ZIP");
        fs::rename(archive, &archive_path).expect("rename archive");

        let extracted = extract_here(&archive_path).expect("extract here");

        assert_eq!(extracted, root.join("report v1"));
        assert_eq!(
            fs::read(extracted.join("hello.txt")).expect("read"),
            b"hello"
        );
        assert!(!root.join("hello.txt").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extract_here_uses_a_new_folder_for_tar_gz_and_tgz() {
        let (root, source) = fixture("extract-here-tar");
        let archive = compress(&source, ArchiveFormat::TarGz).expect("compress");
        let (tar_gz, tgz) = (root.join("report.TAR.GZ"), root.join("report.tgz"));
        fs::rename(source, root.join("input")).expect("relocate original contents");
        fs::rename(archive, &tar_gz).expect("rename archive");
        fs::copy(&tar_gz, &tgz).expect("copy archive");
        let existing = root.join("report");
        fs::create_dir(&existing).expect("create existing folder");
        fs::write(existing.join("original.txt"), b"original").expect("seed existing folder");

        for (source, index) in [(&tar_gz, 1), (&tgz, 2)] {
            let extracted = extract_here(source).expect("extract here");
            assert_eq!(extracted, root.join(format!("report ({index})")));
            assert_eq!(
                fs::read(extracted.join("sample/nested/data.txt")).expect("read nested file"),
                b"data"
            );
        }
        assert_eq!(
            fs::read(existing.join("original.txt")).expect("read"),
            b"original"
        );
        assert!(!existing.join("sample").exists());
        assert!(!root.join("sample").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn zip_progress_reports_destination_and_payload_in_both_directions() {
        let (root, source) = fixture("zip-progress");
        let expected_archive = root.join("sample.zip");
        let mut samples = Vec::new();
        let archive = compress_with_progress(&source, ArchiveFormat::Zip, |event| match event {
            ArchiveProgress::Destination(path) => assert_eq!(path, expected_archive),
            ArchiveProgress::Bytes {
                phase: ArchivePhase::Working,
                processed,
                total,
            } => {
                samples.push((processed, total));
            }
            _ => {}
        })
        .expect("compress with progress");
        assert_eq!(samples.first(), Some(&(0, Some(9))));
        assert_eq!(samples.last(), Some(&(9, Some(9))));

        samples.clear();
        let expected_folder = root.join("sample (1)");
        let extracted = extract_here_with_progress(&archive, |event| match event {
            ArchiveProgress::Destination(path) => assert_eq!(path, expected_folder),
            ArchiveProgress::Bytes {
                phase: ArchivePhase::Working,
                processed,
                total,
            } => {
                samples.push((processed, total));
            }
            _ => {}
        })
        .expect("extract here with progress");
        assert_eq!(extracted, expected_folder);
        assert_eq!(samples.first(), Some(&(0, Some(9))));
        assert_eq!(samples.last(), Some(&(9, Some(9))));
        assert_eq!(
            fs::read(extracted.join("sample/hello.txt")).unwrap(),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn tar_gz_progress_covers_single_file_compression_and_both_read_phases() {
        let (root, source) = fixture("tar-gz-progress");
        let source = source.join("hello.txt");
        let mut tar_bytes = 0;
        let archive = compress_with_progress(&source, ArchiveFormat::TarGz, |event| {
            if let ArchiveProgress::Bytes {
                phase: ArchivePhase::Working,
                processed,
                total,
            } = event
            {
                assert!(total.is_none());
                tar_bytes = processed;
            }
        })
        .expect("compress a single file");
        assert!(tar_bytes > 5);

        let length = fs::metadata(&archive).unwrap().len();
        let mut phases = Vec::new();
        let destination = root.join("extracted");
        extract_with_progress(&archive, &destination, |event| {
            if let ArchiveProgress::Bytes {
                phase,
                processed,
                total,
            } = event
            {
                assert_eq!(total, Some(length));
                if processed == 0 || phases.last() != Some(&phase) {
                    phases.push(phase);
                }
            }
        })
        .expect("extract to a chosen directory");
        assert_eq!(phases, [ArchivePhase::Checking, ArchivePhase::Working]);
        assert_eq!(fs::read(destination.join("hello.txt")).unwrap(), b"hello");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn zip_round_trip_keeps_folder_structure() {
        let (root, source) = fixture("zip");
        let archive = compress(&source, ArchiveFormat::Zip).expect("compress");
        let destination = root.join("extracted");
        extract(&archive, &destination).expect("extract");

        assert_eq!(
            fs::read(destination.join("sample").join("hello.txt")).expect("read"),
            b"hello"
        );
        assert_eq!(
            fs::read(destination.join("sample").join("nested").join("data.txt"))
                .expect("read nested"),
            b"data"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn zip_round_trip_keeps_utf8_chinese_names() {
        let root = temporary_root("utf8-chinese");
        fs::create_dir_all(&root).expect("create fixture root");
        let archive_path = root.join("utf8.zip");
        let file = File::create(&archive_path).expect("create archive");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file("中文目录/说明.txt", SimpleFileOptions::default())
            .expect("start UTF-8 entry");
        writer.write_all(b"hello").expect("write entry");
        writer.finish().expect("finish archive");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract");
        assert_eq!(
            fs::read(destination.join("中文目录").join("说明.txt")).expect("read UTF-8 entry"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extracts_gbk_zip_names_without_utf8_flag() {
        let root = temporary_root("gbk-chinese");
        let name = "珠海开放大学/计算机科学与技术/课程说明.txt";
        let (raw, _, had_errors) = encoding_rs::GBK.encode(name);
        assert!(!had_errors);
        let archive_path = zip_with_raw_entry_name(&root, &raw, b"hello");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract GBK entry");
        assert_eq!(
            fs::read(destination.join(name)).expect("read GBK entry"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn zip_unicode_path_extra_field_takes_precedence() {
        let root = temporary_root("unicode-path-extra");
        let raw_name = b"legacy.txt";
        let unicode_name = "中文目录/说明.txt";
        let archive_path = zip_with_unicode_path_extra(&root, raw_name, unicode_name, b"hello");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract Unicode Path entry");
        assert_eq!(
            fs::read(destination.join(unicode_name)).expect("read Unicode Path entry"),
            b"hello"
        );
        assert!(!destination.join("legacy.txt").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extracts_utf8_zip_names_without_utf8_flag() {
        let root = temporary_root("unflagged-utf8");
        let name = "中文目录/说明.txt";
        let archive_path = zip_with_raw_entry_name(&root, name.as_bytes(), b"hello");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract unflagged UTF-8 entry");
        assert_eq!(
            fs::read(destination.join(name)).expect("read unflagged UTF-8 entry"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preserves_cp437_names_when_they_are_not_gbk() {
        let root = temporary_root("cp437");
        let archive_path = zip_with_raw_entry_name(&root, b"Caf\x82.txt", b"hello");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract CP437 entry");
        assert_eq!(
            fs::read(destination.join("Café.txt")).expect("read CP437 entry"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preserves_plausible_cp437_names_when_raw_bytes_also_form_gbk() {
        let root = temporary_root("ambiguous-cp437");
        let archive_path = zip_with_raw_entry_name(&root, b"\x82\x85.txt", b"hello");

        let destination = root.join("extracted");
        extract(&archive_path, &destination).expect("extract CP437 entry");
        assert_eq!(
            fs::read(destination.join("éà.txt")).expect("read CP437 entry"),
            b"hello"
        );
        assert!(!destination.join("倕.txt").exists());
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn zip_with_raw_entry_name(root: &Path, raw_name: &[u8], content: &[u8]) -> PathBuf {
        fs::create_dir_all(root).expect("create fixture root");
        let archive_path = root.join("legacy.zip");
        let placeholder = "x".repeat(raw_name.len());
        let file = File::create(&archive_path).expect("create archive");
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(&placeholder, SimpleFileOptions::default())
            .expect("start placeholder entry");
        writer.write_all(content).expect("write entry");
        writer.finish().expect("finish archive");

        let original = fs::read(&archive_path).expect("read archive");
        let mut patched = Vec::with_capacity(original.len());
        let mut matches = 0;
        let mut index = 0;
        while index < original.len() {
            if original[index..].starts_with(placeholder.as_bytes()) {
                patched.extend_from_slice(raw_name);
                index += placeholder.len();
                matches += 1;
            } else {
                patched.push(original[index]);
                index += 1;
            }
        }
        assert_eq!(
            matches, 2,
            "local and central ZIP headers should contain the placeholder"
        );
        fs::write(&archive_path, patched).expect("write patched archive");
        archive_path
    }

    fn crc32_ieee(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 0 {
                    crc >> 1
                } else {
                    (crc >> 1) ^ 0xEDB8_8320
                };
            }
        }
        !crc
    }

    fn zip_with_unicode_path_extra(
        root: &Path,
        raw_name: &[u8],
        unicode_name: &str,
        content: &[u8],
    ) -> PathBuf {
        fs::create_dir_all(root).expect("create fixture root");
        let archive_path = root.join("unicode-path.zip");
        let content_crc = crc32_ieee(content);
        let mut unicode_path = vec![1];
        unicode_path.extend_from_slice(&crc32_ieee(raw_name).to_le_bytes());
        unicode_path.extend_from_slice(unicode_name.as_bytes());
        let mut extra = Vec::new();
        extra.extend_from_slice(&0x7075u16.to_le_bytes());
        extra.extend_from_slice(&(unicode_path.len() as u16).to_le_bytes());
        extra.extend_from_slice(&unicode_path);

        let mut local = Vec::new();
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&content_crc.to_le_bytes());
        local.extend_from_slice(&(content.len() as u32).to_le_bytes());
        local.extend_from_slice(&(content.len() as u32).to_le_bytes());
        local.extend_from_slice(&(raw_name.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(raw_name);
        local.extend_from_slice(content);

        let central_offset = local.len() as u32;
        let mut central = Vec::new();
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&content_crc.to_le_bytes());
        central.extend_from_slice(&(content.len() as u32).to_le_bytes());
        central.extend_from_slice(&(content.len() as u32).to_le_bytes());
        central.extend_from_slice(&(raw_name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(raw_name);
        central.extend_from_slice(&extra);

        let central_size = central.len() as u32;
        let mut archive = local;
        archive.extend_from_slice(&central);
        archive.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        archive.extend_from_slice(&0u16.to_le_bytes());
        archive.extend_from_slice(&0u16.to_le_bytes());
        archive.extend_from_slice(&1u16.to_le_bytes());
        archive.extend_from_slice(&1u16.to_le_bytes());
        archive.extend_from_slice(&central_size.to_le_bytes());
        archive.extend_from_slice(&central_offset.to_le_bytes());
        archive.extend_from_slice(&0u16.to_le_bytes());
        fs::write(&archive_path, archive).expect("write Unicode Path archive");
        archive_path
    }

    #[test]
    fn tar_gz_round_trip_keeps_folder_structure() {
        let (root, source) = fixture("tar");
        let archive = compress(&source, ArchiveFormat::TarGz).expect("compress");
        let destination = root.join("extracted");
        extract(&archive, &destination).expect("extract");

        assert_eq!(
            fs::read(destination.join("sample").join("hello.txt")).expect("read"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn compression_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let (root, source) = fixture("symlink");
        symlink(source.join("hello.txt"), source.join("link.txt")).expect("create symlink");

        let zip_error = compress(&source, ArchiveFormat::Zip).expect_err("reject zip symlink");
        let tar_error = compress(&source, ArchiveFormat::TarGz).expect_err("reject tar symlink");
        assert!(zip_error.contains("符号链接"));
        assert!(tar_error.contains("符号链接"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn extraction_does_not_overwrite_existing_files() {
        let (root, source) = fixture("existing");
        let archive = compress(&source, ArchiveFormat::Zip).expect("compress");
        let destination = root.join("extracted");
        fs::create_dir_all(destination.join("sample")).expect("create destination");
        fs::write(destination.join("sample").join("hello.txt"), b"old").expect("write old file");

        let error = extract(&archive, &destination).expect_err("must reject overwrite");
        assert!(error.contains("已经存在"));
        assert_eq!(
            fs::read(destination.join("sample").join("hello.txt")).expect("read old"),
            b"old"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
