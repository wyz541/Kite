use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const MAX_TEXT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextLineEnding {
    Lf,
    CrLf,
}

impl TextLineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::CrLf => "crlf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "lf" => Some(Self::Lf),
            "crlf" => Some(Self::CrLf),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDocument {
    pub path: PathBuf,
    pub text: String,
    pub language: String,
    pub line_ending: TextLineEnding,
    pub has_bom: bool,
    pub content_sha256: String,
    pub byte_len: u64,
    source: DocumentSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DocumentSource {
    PlainText,
    ZipEntry { entry: String },
}

impl TextDocument {
    pub fn open(path: &Path) -> Result<Self, String> {
        let metadata =
            fs::metadata(path).map_err(|error| format!("无法读取文本文件信息：{error}"))?;
        if !metadata.is_file() {
            return Err("目标不是文件".to_string());
        }
        if metadata.len() > MAX_TEXT_FILE_BYTES {
            return Err(format!(
                "文件超过 {} MB，文本编辑器不会打开。",
                MAX_TEXT_FILE_BYTES / 1024 / 1024
            ));
        }

        let bytes = fs::read(path).map_err(|error| format!("无法读取文本文件：{error}"))?;
        decode_text(path, &bytes, metadata.len(), DocumentSource::PlainText)
    }

    /// Opens a plain text file or the editable XML/JSON payload inside a
    /// supported document package. The package itself remains the save target.
    pub fn open_editable(path: &Path) -> Result<Self, String> {
        if !is_packaged_document_path(path) {
            return Self::open(path);
        }
        let bytes = read_package(path)?;
        let Some(entry) = package_entry(path, &bytes)? else {
            return Err("文档包中没有可编辑的内容".to_string());
        };
        let text = entry.1;
        let mut document = decode_text(
            path,
            &text,
            bytes.len() as u64,
            DocumentSource::ZipEntry { entry: entry.0 },
        )?;
        document.content_sha256 = hash_bytes(&bytes);
        Ok(document)
    }

    pub fn save(&self, text: &str) -> Result<Self, String> {
        match &self.source {
            DocumentSource::PlainText => save_text_document(
                &self.path,
                text,
                self.line_ending,
                self.has_bom,
                &self.content_sha256,
            ),
            DocumentSource::ZipEntry { entry } => {
                if text.as_bytes().contains(&0) {
                    return Err("文本包含空字节，Kite 已阻止保存。".to_string());
                }
                if entry.ends_with(".json") {
                    serde_json::from_str::<serde_json::Value>(text)
                        .map_err(|error| format!("文档 JSON 无效：{error}"))?;
                } else {
                    parse_document_xml(text)?;
                }
                let bytes = encode_text(text, self.line_ending, self.has_bom);
                save_zip_entry(&self.path, entry, &bytes, &self.content_sha256)
            }
        }
    }
}

pub fn is_packaged_document_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("xmind" | "docx" | "pptx")
    )
}

pub fn is_mind_map_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("mm" | "freemind" | "opml" | "opmlm")
    )
}

pub fn is_editable_preview_path(path: &Path) -> bool {
    is_text_path(path) || is_packaged_document_path(path) || is_mind_map_path(path)
}

pub fn editable_document_limit(path: &Path) -> u64 {
    if is_packaged_document_path(path) {
        MAX_PACKAGE_BYTES
    } else {
        MAX_TEXT_FILE_BYTES
    }
}

/// Returns a bounded human-readable preview for text, mind-map and Office-like files.
pub fn read_preview_text(path: &Path) -> Result<String, String> {
    if is_packaged_document_path(path) {
        let bytes = read_package(path)?;
        let Some((entry, payload)) = package_entry(path, &bytes)? else {
            return Err("文档包中没有可预览的内容".to_string());
        };
        let text = String::from_utf8_lossy(&payload);
        let content = match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "docx" | "pptx" => extract_xml_text(&text)?,
            "xmind" if entry.ends_with(".json") => serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_else(|| text.into_owned()),
            _ => text.into_owned(),
        };
        return bounded_preview(&content);
    }
    crate::preview::read_text_preview(path)
}

pub fn save_text_document(
    path: &Path,
    text: &str,
    line_ending: TextLineEnding,
    has_bom: bool,
    expected_sha256: &str,
) -> Result<TextDocument, String> {
    if text.as_bytes().contains(&0) {
        return Err("文本包含空字节，Kite 已阻止保存。".to_string());
    }

    let original = fs::read(path).map_err(|error| format!("无法检查原文件状态：{error}"))?;
    let current_hash = hash_bytes(&original);
    if !expected_sha256.is_empty() && current_hash != expected_sha256 {
        return Err("文件已在 Kite 外被修改。请重新打开后再保存。".to_string());
    }

    let bytes = encode_text(text, line_ending, has_bom);
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(&bytes))
        .map_err(|error| format!("无法保存文本文件：{error}"))?;
    TextDocument::open(path)
}

pub fn is_text_path(path: &Path) -> bool {
    let language = language_for_path(path);
    if language != "text" {
        return true;
    }
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    extension.is_empty()
        || matches!(
            extension.as_str(),
            "txt"
                | "log"
                | "ini"
                | "conf"
                | "cfg"
                | "properties"
                | "env"
                | "csv"
                | "tsv"
                | "svg"
                | "xml"
                | "bat"
                | "cmd"
                | "ps1"
                | "psm1"
                | "psd1"
        )
        || matches!(
            name.as_str(),
            ".gitignore" | ".gitattributes" | ".gitmodules" | ".editorconfig"
        )
}

pub fn is_markdown_path(path: &Path) -> bool {
    crate::plugins::builtin_plugins()
        .iter()
        .any(|plugin| plugin.supports_path(path))
}

pub fn language_for_path(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match name.as_str() {
        "dockerfile" => return "dockerfile".to_string(),
        "makefile" | "gnumakefile" => return "make".to_string(),
        ".gitignore" | ".gitattributes" | ".gitmodules" => return "text".to_string(),
        _ => {}
    }

    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let language = match extension.as_str() {
        "rs" => "rust",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "py" | "pyw" => "python",
        "json" | "jsonc" | "xmind" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ if is_markdown_path(path) => "markdown",
        "html" | "htm" => "html",
        "mm" | "freemind" | "opml" | "opmlm" | "docx" | "pptx" => "xml",
        "css" | "scss" | "less" => "css",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "go" => "go",
        "kt" | "kts" => "kotlin",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "swift" => "swift",
        "lua" => "lua",
        "rb" => "ruby",
        "php" => "php",
        "proto" => "proto",
        "cmake" => "cmake",
        "diff" | "patch" => "diff",
        _ => "text",
    };
    language.to_string()
}

fn decode_text(
    path: &Path,
    bytes: &[u8],
    byte_len: u64,
    source: DocumentSource,
) -> Result<TextDocument, String> {
    let (has_bom, content) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(content) => (true, content),
        None => (false, bytes),
    };
    if content.contains(&0) {
        return Err("此文件不是 UTF-8 文本文件。".to_string());
    }
    let text =
        std::str::from_utf8(content).map_err(|_| "此文件不是 UTF-8 文本文件。".to_string())?;
    let line_ending = detect_line_ending(text);
    Ok(TextDocument {
        path: path.to_path_buf(),
        text: normalize_line_endings(text),
        language: language_for_path(path),
        line_ending,
        has_bom,
        content_sha256: hash_bytes(bytes),
        byte_len,
        source,
    })
}

fn read_package(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("无法打开文档包：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法读取文档包信息：{error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_PACKAGE_BYTES {
        return Err("目标不是普通文件，或文档包超过 64 MiB。".to_string());
    }
    let mut bytes = Vec::new();
    file.take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取文档包：{error}"))?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        return Err("文档包超过 64 MiB。".to_string());
    }
    Ok(bytes)
}

fn package_entry(path: &Path, bytes: &[u8]) -> Result<Option<(String, Vec<u8>)>, String> {
    if !is_packaged_document_path(path) {
        return Ok(None);
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("无法读取文档包：{error}"))?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let preferred = match extension.as_str() {
        "xmind" => ["content.json", "content.xml"].as_slice(),
        "docx" => ["word/document.xml"].as_slice(),
        "pptx" => ["ppt/slides/slide1.xml"].as_slice(),
        _ => &[] as &[&str],
    };
    let mut selected = preferred.iter().find_map(|candidate| {
        archive
            .by_name(candidate)
            .ok()
            .map(|entry| entry.name().to_string())
    });
    if selected.is_none() && extension == "pptx" {
        selected = (1..=archive.len()).find_map(|index| {
            archive.by_index(index - 1).ok().and_then(|entry| {
                let name = entry.name().to_string();
                (name.starts_with("ppt/slides/slide") && name.ends_with(".xml")).then_some(name)
            })
        });
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let entry = archive
        .by_name(&selected)
        .map_err(|error| format!("无法读取文档内容：{error}"))?;
    if entry.size() > MAX_TEXT_FILE_BYTES {
        return Err("文档中的可编辑内容超过 8 MiB。".to_string());
    }
    let mut payload = Vec::new();
    entry
        .take(MAX_TEXT_FILE_BYTES + 1)
        .read_to_end(&mut payload)
        .map_err(|error| format!("无法读取文档内容：{error}"))?;
    if payload.len() as u64 > MAX_TEXT_FILE_BYTES {
        return Err("文档中的可编辑内容超过 8 MiB。".to_string());
    }
    Ok(Some((selected, payload)))
}

fn save_zip_entry(
    path: &Path,
    target_entry: &str,
    bytes: &[u8],
    expected_sha256: &str,
) -> Result<TextDocument, String> {
    if bytes.len() as u64 > MAX_TEXT_FILE_BYTES {
        return Err("文档中的可编辑内容超过 8 MiB。".to_string());
    }
    let original = read_package(path)?;
    if hash_bytes(&original) != expected_sha256 {
        return Err("文件已在 Kite 外被修改。请重新打开后再保存。".to_string());
    }
    let mut archive = ZipArchive::new(Cursor::new(&original))
        .map_err(|error| format!("无法读取文档包：{error}"))?;
    let mut output = Cursor::new(Vec::with_capacity(original.len()));
    {
        let mut writer = ZipWriter::new(&mut output);
        for index in 0..archive.len() {
            let entry = archive
                .by_index_raw(index)
                .map_err(|error| format!("无法读取文档包条目：{error}"))?;
            if entry.name() == target_entry {
                let options =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
                writer
                    .start_file(target_entry, options)
                    .map_err(|error| format!("无法写入文档包：{error}"))?;
                writer
                    .write_all(bytes)
                    .map_err(|error| format!("无法写入文档内容：{error}"))?;
            } else {
                writer
                    .raw_copy_file(entry)
                    .map_err(|error| format!("无法复制文档包条目：{error}"))?;
            }
        }
        writer
            .finish()
            .map_err(|error| format!("无法完成文档包写入：{error}"))?;
    }
    if output.get_ref().len() as u64 > MAX_PACKAGE_BYTES {
        return Err("修改后的文档包超过 64 MiB。".to_string());
    }
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(output.get_ref()))
        .map_err(|error| format!("无法保存文档包：{error}"))?;
    TextDocument::open_editable(path)
}

fn parse_document_xml(xml: &str) -> Result<roxmltree::Document<'_>, String> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            nodes_limit: 250_000,
            ..Default::default()
        },
    )
    .map_err(|error| format!("文档 XML 无效或超过结构限制：{error}"))
}

fn extract_xml_text(xml: &str) -> Result<String, String> {
    let document = parse_document_xml(xml)?;
    Ok(document
        .descendants()
        .filter(|node| {
            node.is_text() && node.parent().is_some_and(|parent| parent.has_tag_name("t"))
        })
        .filter_map(|node| node.text())
        .collect::<Vec<_>>()
        .join("\n"))
}

fn bounded_preview(text: &str) -> Result<String, String> {
    let mut chars = text.chars();
    let mut excerpt = String::new();
    excerpt.extend(chars.by_ref().take(crate::preview::TEXT_PREVIEW_CHAR_LIMIT));
    if chars.next().is_some() {
        excerpt.push('…');
    }
    Ok(if excerpt.trim().is_empty() {
        "文件没有可显示的文本内容。".to_string()
    } else {
        excerpt
    })
}

fn detect_line_ending(text: &str) -> TextLineEnding {
    let crlf = text.match_indices("\r\n").count();
    let lf = text.bytes().filter(|byte| *byte == b'\n').count();
    if crlf > 0 && crlf == lf {
        TextLineEnding::CrLf
    } else {
        TextLineEnding::Lf
    }
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn encode_text(text: &str, line_ending: TextLineEnding, has_bom: bool) -> Vec<u8> {
    let normalized = normalize_line_endings(text);
    let serialized = match line_ending {
        TextLineEnding::Lf => normalized,
        TextLineEnding::CrLf => normalized.replace('\n', "\r\n"),
    };
    let mut bytes = Vec::with_capacity(serialized.len() + usize::from(has_bom) * 3);
    if has_bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(serialized.as_bytes());
    bytes
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{MAX_TEXT_FILE_BYTES, TextLineEnding, language_for_path, save_text_document};

    fn temporary_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "kite-text-editor-{label}-{}-{unique}.txt",
            std::process::id()
        ))
    }

    #[test]
    fn detects_utf8_bom_crlf_and_language() {
        let path = temporary_path("detect").with_extension("rs");
        fs::write(&path, b"\xEF\xBB\xBFfn main() {\r\n}\r\n").expect("write fixture");
        let document = super::TextDocument::open(&path).expect("open document");
        assert!(document.has_bom);
        assert_eq!(document.line_ending, TextLineEnding::CrLf);
        assert_eq!(document.text, "fn main() {\n}\n");
        assert_eq!(document.language, "rust");
        assert_eq!(language_for_path(&path), "rust");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recognizes_mk_and_markdown_without_promoting_makefiles() {
        use std::path::Path;

        for name in [
            "notes.mk",
            "notes.MK",
            "notes.MD",
            "notes.markdown",
            "notes.mdx",
        ] {
            let path = Path::new(name);
            assert!(super::is_markdown_path(path));
            assert!(super::is_editable_preview_path(path));
            assert_eq!(language_for_path(path), "markdown");
        }
        assert!(!super::is_markdown_path(Path::new("Makefile")));
        assert!(!super::is_markdown_path(Path::new("notes.txt")));
    }

    #[test]
    fn save_preserves_bom_and_crlf() {
        let path = temporary_path("save");
        fs::write(&path, b"\xEF\xBB\xBFfirst\r\nsecond\r\n").expect("write fixture");
        let document = super::TextDocument::open(&path).expect("open document");
        let saved = document
            .save("first\nsecond\nthird\n")
            .expect("save document");
        assert!(saved.has_bom);
        assert_eq!(saved.line_ending, TextLineEnding::CrLf);
        assert_eq!(
            fs::read(&path).expect("read saved file"),
            b"\xEF\xBB\xBFfirst\r\nsecond\r\nthird\r\n"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_an_external_change() {
        let path = temporary_path("conflict");
        fs::write(&path, "original").expect("write fixture");
        let document = super::TextDocument::open(&path).expect("open document");
        fs::write(&path, "changed outside Kite").expect("modify fixture");
        let error = save_text_document(
            &path,
            "Kite change",
            document.line_ending,
            document.has_bom,
            &document.content_sha256,
        )
        .expect_err("external change must fail");
        assert!(error.contains("Kite 外被修改"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_binary_and_oversized_files() {
        let binary = temporary_path("binary");
        fs::write(&binary, b"text\0binary").expect("write fixture");
        assert!(super::TextDocument::open(&binary).is_err());
        let _ = fs::remove_file(binary);

        let oversized = temporary_path("oversized");
        let file = fs::File::create(&oversized).expect("create fixture");
        file.set_len(MAX_TEXT_FILE_BYTES + 1)
            .expect("extend fixture");
        let error = super::TextDocument::open(&oversized).expect_err("oversized file must fail");
        assert!(error.contains("8 MB"));
        let _ = fs::remove_file(oversized);
    }

    #[test]
    fn opens_and_saves_xmind_content_entry() {
        use std::io::{Read, Write};
        use zip::write::SimpleFileOptions;
        use zip::{ZipArchive, ZipWriter};

        let path = temporary_path("xmind").with_extension("xmind");
        let mut output = fs::File::create(&path).expect("create fixture");
        let mut writer = ZipWriter::new(&mut output);
        writer
            .start_file("mimetype", SimpleFileOptions::default())
            .expect("start mimetype");
        writer
            .write_all(b"application/xmind")
            .expect("write mimetype");
        writer
            .start_file("content.json", SimpleFileOptions::default())
            .expect("start content");
        writer
            .write_all(br#"{"title":"before"}"#)
            .expect("write content");
        writer.finish().expect("finish fixture");
        drop(output);

        let document = super::TextDocument::open_editable(&path).expect("open xmind");
        assert!(document.text.contains("before"));
        assert!(super::read_preview_text(&path).unwrap().contains("before"));
        assert!(document.save("{").is_err());
        document.save(r#"{"title":"after"}"#).expect("save xmind");

        let file = fs::File::open(&path).expect("open saved fixture");
        let mut archive = ZipArchive::new(file).expect("read saved fixture");
        assert_eq!(archive.by_index(0).expect("first entry").name(), "mimetype");
        let mut content = String::new();
        archive
            .by_name("content.json")
            .expect("content entry")
            .read_to_string(&mut content)
            .expect("read content entry");
        assert!(content.contains("after"));
        drop(archive);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn office_preview_extracts_text_and_rejects_invalid_xml() {
        let xml = r#"<w:document xmlns:w="urn:test"><w:p><w:r><w:t>One &amp; two</w:t></w:r></w:p></w:document>"#;
        assert_eq!(super::extract_xml_text(xml).unwrap(), "One & two");
        assert!(super::parse_document_xml("<document>").is_err());
    }
}
