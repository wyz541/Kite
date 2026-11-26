//! Modern `.xmind` package access with lossless preservation of unknown ZIP entries.
//!
//! This module intentionally treats `content.json` as the only parsed entry in the
//! first compatibility slice. Every other entry is copied through the ZIP writer
//! without decoding so that future XMind features and attachments survive a save.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

pub const CONTENT_JSON_ENTRY: &str = "content.json";
pub const CONTENT_XML_ENTRY: &str = "content.xml";
pub const MAX_XMIND_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_CONTENT_JSON_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum XmindError {
    #[error("无法读取 XMind 文件：{0}")]
    Io(#[from] std::io::Error),
    #[error("无法读取 XMind ZIP 容器：{0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("无法解析 content.json：{0}")]
    Json(#[from] serde_json::Error),
    #[error("XMind 文件超过 {MAX_XMIND_PACKAGE_BYTES} 字节上限")]
    PackageTooLarge,
    #[error("content.json 超过 {MAX_CONTENT_JSON_BYTES} 字节上限")]
    ContentTooLarge,
    #[error("XMind 文件缺少 content.json")]
    MissingContentJson,
    #[error(
        "检测到仅含 content.xml 的 XMind 8 文件；当前切片只支持读取和迁移入口，暂不解析 Legacy 格式"
    )]
    LegacyContentXml,
    #[error("content.json 根节点必须是数组")]
    InvalidContentShape,
    #[error("content.json 根数组不能为空")]
    EmptyContent,
    #[error("content.json 条目重复")]
    DuplicateContentJson,
    #[error("目标不是普通文件：{0}")]
    NotAFile(PathBuf),
    #[error("文件已在 Kite 外被修改，请重新打开后再保存")]
    ExternalModification,
    #[error("无法保存 XMind 文件：{0}")]
    Save(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmindFormat {
    ModernJson,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmindEntryInfo {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmindSummary {
    pub format: XmindFormat,
    pub sheet_count: usize,
    pub entry_count: usize,
    pub source_sha256: String,
    pub content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmindSaveReport {
    pub bytes_written: usize,
    pub byte_identical: bool,
    pub rewritten_entries: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct XmindPackage {
    source_path: Option<PathBuf>,
    source_bytes: Vec<u8>,
    source_sha256: String,
    source_content: Value,
    content: Value,
    entries: Vec<XmindEntryInfo>,
    dirty_entries: BTreeMap<String, Vec<u8>>,
}

impl XmindPackage {
    pub fn open(path: &Path) -> Result<Self, XmindError> {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(XmindError::NotAFile(path.to_path_buf()));
        }
        if metadata.len() > MAX_XMIND_PACKAGE_BYTES {
            return Err(XmindError::PackageTooLarge);
        }

        let file = File::open(path)?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_XMIND_PACKAGE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_XMIND_PACKAGE_BYTES {
            return Err(XmindError::PackageTooLarge);
        }

        let mut package = Self::from_bytes(bytes)?;
        package.source_path = Some(dunce::canonicalize(path)?);
        Ok(package)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, XmindError> {
        if bytes.len() as u64 > MAX_XMIND_PACKAGE_BYTES {
            return Err(XmindError::PackageTooLarge);
        }

        let mut archive = ZipArchive::new(Cursor::new(&bytes))?;
        let mut entries = Vec::with_capacity(archive.len());
        let mut content_json_index = None;
        let mut has_legacy_content_xml = false;

        for index in 0..archive.len() {
            let entry = archive.by_index_raw(index)?;
            let name = entry.name().to_string();
            if name == CONTENT_JSON_ENTRY {
                if content_json_index.replace(index).is_some() {
                    return Err(XmindError::DuplicateContentJson);
                }
            } else if name == CONTENT_XML_ENTRY {
                has_legacy_content_xml = true;
            }
            entries.push(XmindEntryInfo {
                name,
                size: entry.size(),
                compressed_size: entry.compressed_size(),
                is_dir: entry.is_dir(),
            });
        }

        let content_bytes = match content_json_index {
            Some(index) => {
                let entry = archive.by_index(index)?;
                if entry.size() > MAX_CONTENT_JSON_BYTES {
                    return Err(XmindError::ContentTooLarge);
                }
                let mut content = Vec::with_capacity(entry.size() as usize);
                entry
                    .take(MAX_CONTENT_JSON_BYTES + 1)
                    .read_to_end(&mut content)?;
                if content.len() as u64 > MAX_CONTENT_JSON_BYTES {
                    return Err(XmindError::ContentTooLarge);
                }
                content
            }
            None if has_legacy_content_xml => return Err(XmindError::LegacyContentXml),
            None => return Err(XmindError::MissingContentJson),
        };

        let content: Value = serde_json::from_slice(&content_bytes)?;
        validate_content_shape(&content)?;

        let source_sha256 = hash_bytes(&bytes);
        Ok(Self {
            source_path: None,
            source_bytes: bytes,
            source_sha256,
            source_content: content.clone(),
            content,
            entries,
            dirty_entries: BTreeMap::new(),
        })
    }

    pub fn summary(&self) -> XmindSummary {
        let sheet_count = self.content.as_array().map_or(0, Vec::len);
        XmindSummary {
            format: XmindFormat::ModernJson,
            sheet_count,
            entry_count: self.entries.len(),
            source_sha256: self.source_sha256.clone(),
            content_sha256: hash_bytes(self.content.to_string().as_bytes()),
        }
    }

    pub fn content(&self) -> &Value {
        &self.content
    }

    pub fn entries(&self) -> &[XmindEntryInfo] {
        &self.entries
    }

    pub fn is_dirty(&self) -> bool {
        self.content != self.source_content || !self.dirty_entries.is_empty()
    }

    pub fn edit_content_json<T>(
        &mut self,
        edit: impl FnOnce(&mut Value) -> Result<T, XmindError>,
    ) -> Result<T, XmindError> {
        let mut next = self.content.clone();
        let result = edit(&mut next)?;
        self.replace_content(next);
        Ok(result)
    }

    pub fn replace_content(&mut self, content: Value) {
        self.content = content;
    }

    pub fn save(&mut self) -> Result<XmindSaveReport, XmindError> {
        let path = self
            .source_path
            .clone()
            .ok_or_else(|| XmindError::Save("当前文档没有源路径".to_string()))?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<XmindSaveReport, XmindError> {
        if self.source_path.as_deref() == Some(path)
            || fs::canonicalize(path)
                .ok()
                .zip(self.source_path.as_ref())
                .is_some_and(|(candidate, source)| candidate == *source)
        {
            let current = fs::read(path)?;
            if hash_bytes(&current) != self.source_sha256 {
                return Err(XmindError::ExternalModification);
            }
        }

        let dirty_content = self.content != self.source_content;
        let rewritten_entries = if dirty_content {
            vec![CONTENT_JSON_ENTRY.to_string()]
        } else {
            Vec::new()
        };
        let bytes = if self.dirty_entries.is_empty() && !dirty_content {
            self.source_bytes.clone()
        } else {
            let mut replacements = self.dirty_entries.clone();
            if dirty_content {
                replacements.insert(
                    CONTENT_JSON_ENTRY.to_string(),
                    serde_json::to_vec(&self.content)?,
                );
            }
            self.rebuild_with_replacements(&replacements)?
        };
        let byte_identical = bytes == self.source_bytes;

        AtomicFile::new(path, AllowOverwrite)
            .write(|file| file.write_all(&bytes))
            .map_err(|error| XmindError::Save(error.to_string()))?;

        self.source_path = Some(dunce::canonicalize(path)?);
        self.source_bytes = bytes.clone();
        self.source_sha256 = hash_bytes(&bytes);
        self.source_content = self.content.clone();
        self.dirty_entries.clear();

        Ok(XmindSaveReport {
            bytes_written: bytes.len(),
            byte_identical,
            rewritten_entries,
        })
    }

    fn rebuild_with_replacements(
        &self,
        replacements: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>, XmindError> {
        let mut archive = ZipArchive::new(Cursor::new(&self.source_bytes))?;
        let mut output = Cursor::new(Vec::with_capacity(self.source_bytes.len()));
        let mut written = BTreeMap::<String, bool>::new();

        {
            let mut writer = ZipWriter::new(&mut output);
            for index in 0..archive.len() {
                let entry = archive.by_index_raw(index)?;
                let name = entry.name().to_string();
                if let Some(bytes) = replacements.get(&name) {
                    let mut options =
                        SimpleFileOptions::default().compression_method(entry.compression());
                    if let Some(mode) = entry.unix_mode() {
                        options = options.unix_permissions(mode);
                    }
                    if let Some(last_modified) = entry.last_modified() {
                        options = options.last_modified_time(last_modified);
                    }
                    writer.start_file(&name, options)?;
                    writer.write_all(bytes)?;
                    written.insert(name, true);
                } else {
                    writer.raw_copy_file(entry)?;
                }
            }

            for (name, bytes) in replacements {
                if written.get(name).copied().unwrap_or(false) {
                    continue;
                }
                let options = SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                writer.start_file(name, options)?;
                writer.write_all(bytes)?;
            }

            writer.finish()?;
        }

        let bytes = output.into_inner();
        Self::validate_modern_package(&bytes)?;
        Ok(bytes)
    }

    fn validate_modern_package(bytes: &[u8]) -> Result<(), XmindError> {
        let mut archive = ZipArchive::new(Cursor::new(bytes))?;
        let content = archive.by_name(CONTENT_JSON_ENTRY)?;
        let mut payload = Vec::new();
        content
            .take(MAX_CONTENT_JSON_BYTES + 1)
            .read_to_end(&mut payload)?;
        if payload.len() as u64 > MAX_CONTENT_JSON_BYTES {
            return Err(XmindError::ContentTooLarge);
        }
        let content: Value = serde_json::from_slice(&payload)?;
        validate_content_shape(&content)
    }
}

fn validate_content_shape(content: &Value) -> Result<(), XmindError> {
    if !content.is_array() {
        return Err(XmindError::InvalidContentShape);
    }
    if content.as_array().is_some_and(Vec::is_empty) {
        return Err(XmindError::EmptyContent);
    }
    Ok(())
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
