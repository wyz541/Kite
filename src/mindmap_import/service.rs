//! Path-based orchestration for previewing and generating imported mindmaps.
//!
//! Adapters remain responsible for extracting one source format into the shared
//! outline model. This module applies cross-adapter content strategies, builds a
//! bounded preview, validates the generated package, and writes it without ever
//! treating the source document as a destination.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::mindmap::XmindPackage;
use crate::mindmap_import::{
    ConversionOptions, ConversionReport, ConversionStats, ConversionWarning, ConversionWarningCode,
    DocxAdapter, HtmlAdapter, MarkdownAdapter, OpmlAdapter, OutlineAdapter, OutlineDocument,
    OutlineNode, OutlineNodeKind, SourceFormat, SourceInput, TextAdapter, write_xmind_bytes,
    write_xmind_file,
};

const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PREVIEW_NODES: usize = 160;
const MAX_PREVIEW_TITLE_CHARS: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableStrategy {
    /// Keep the table in the owning topic's notes.
    Note,
    /// Add one child topic per table row while retaining header context in notes.
    RowsAsTopics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeStrategy {
    /// Preserve a code block as its own content topic with a note.
    TopicNote,
    /// Fold code block notes into the owning topic.
    ParentNote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageStrategy {
    /// Preserve an image as a content topic carrying its link/reference.
    TopicLink,
    /// Fold image links and attachments into the owning topic's notes.
    ParentNote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentStrategies {
    pub table: TableStrategy,
    pub code: CodeStrategy,
    pub image: ImageStrategy,
}

impl Default for ContentStrategies {
    fn default() -> Self {
        Self {
            table: TableStrategy::Note,
            code: CodeStrategy::TopicNote,
            image: ImageStrategy::TopicLink,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFormatInfo {
    pub id: String,
    pub display_name: String,
    pub extensions: Vec<String>,
    pub supported: bool,
    pub support_note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSourceInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub format: ImportFormatInfo,
    pub default_output_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ConversionPreview {
    pub source: ImportSourceInfo,
    pub output_path: PathBuf,
    pub document_title: String,
    pub source_sha256: String,
    pub report: ConversionReport,
    pub stats: ConversionStats,
    pub structure_confidence: String,
    pub inferred_heading_count: usize,
    pub ambiguous_short_line_count: usize,
    pub sample_tree_json: String,
    pub sampled_node_count: usize,
}

#[derive(Clone, Debug)]
pub struct ConversionOutcome {
    pub output_path: PathBuf,
    pub source_sha256: String,
    pub output_sha256: String,
    pub source_unchanged: bool,
    pub report: ConversionReport,
}

pub fn supported_formats() -> Vec<ImportFormatInfo> {
    [
        (
            "markdown",
            "Markdown",
            &["md", "markdown", "mdx", "mk"][..],
            true,
            "",
        ),
        ("text", "纯文本", &["txt", "text", "log"][..], true, ""),
        ("docx", "Word 文档", &["docx"][..], true, ""),
        ("html", "HTML", &["html", "htm"][..], true, ""),
        ("opml", "OPML 大纲", &["opml"][..], true, ""),
        (
            "pdf",
            "PDF",
            &["pdf"][..],
            false,
            "PDF 适配器尚未接入；请先导出为 DOCX、Markdown 或 TXT。",
        ),
    ]
    .into_iter()
    .map(
        |(id, display_name, extensions, supported, support_note)| ImportFormatInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            extensions: extensions.iter().map(|value| value.to_string()).collect(),
            supported,
            support_note: support_note.to_string(),
        },
    )
    .collect()
}

pub fn detect_source(path: &Path) -> Option<ImportSourceInfo> {
    let source_path = normalized_path(path);
    let extension = source_path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)?;
    let format = supported_formats().into_iter().find(|format| {
        format
            .extensions
            .iter()
            .any(|candidate| candidate == &extension)
    })?;
    let file_name = source_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document")
        .to_string();
    let default_output_path = unique_output_path(&source_path);
    Some(ImportSourceInfo {
        path: source_path,
        file_name,
        format,
        default_output_path,
    })
}

pub fn preview(
    source_path: &Path,
    options: &ConversionOptions,
    strategies: ContentStrategies,
) -> Result<ConversionPreview, crate::mindmap_import::ImportError> {
    let source = required_source(source_path)?;
    let (bytes, source_sha256) = read_source(&source.path)?;
    let document = extract_document(&source, &bytes, options, strategies)?;
    let written = write_xmind_bytes(&document, options)?;
    let sample = sample_tree(&document)?;
    let stats = document.stats.clone();
    let structure_confidence = preview_structure_confidence(&document);
    let inferred_heading_count = original_format_count(&document, "inferred_heading_count");
    let ambiguous_short_line_count = original_format_count(&document, "ambiguous_short_line_count");
    Ok(ConversionPreview {
        source,
        output_path: unique_output_path(source_path),
        document_title: document.title,
        source_sha256,
        report: written.report,
        stats,
        structure_confidence,
        inferred_heading_count,
        ambiguous_short_line_count,
        sample_tree_json: sample.json,
        sampled_node_count: sample.node_count,
    })
}

pub fn generate(
    source_path: &Path,
    output_path: &Path,
    expected_source_sha256: &str,
    overwrite_existing: bool,
    options: &ConversionOptions,
    strategies: ContentStrategies,
) -> Result<ConversionOutcome, crate::mindmap_import::ImportError> {
    let source = required_source(source_path)?;
    let (bytes, source_sha256) = read_source(&source.path)?;
    if !expected_source_sha256.trim().is_empty() && expected_source_sha256 != source_sha256 {
        return Err(crate::mindmap_import::ImportError::InvalidOptions(
            "源文件自预览后已发生变化，请重新生成预览".to_string(),
        ));
    }

    let output_path = validated_output_path(&source.path, output_path, overwrite_existing)?;
    let document = extract_document(&source, &bytes, options, strategies)?;
    let written = write_xmind_file(&document, options, &output_path)?;
    let output_bytes = fs::read(&output_path)?;
    let output_sha256 = sha256(&output_bytes);
    XmindPackage::from_bytes(output_bytes)?;
    let source_unchanged = sha256(&fs::read(&source.path)?) == source_sha256;

    Ok(ConversionOutcome {
        output_path,
        source_sha256,
        output_sha256,
        source_unchanged,
        report: written.report,
    })
}

fn required_source(path: &Path) -> Result<ImportSourceInfo, crate::mindmap_import::ImportError> {
    let source = detect_source(path).ok_or_else(|| {
        crate::mindmap_import::ImportError::UnsupportedFormat(SourceFormat::Other(
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_string(),
        ))
    })?;
    if !source.format.supported {
        return Err(crate::mindmap_import::ImportError::UnsupportedFormat(
            SourceFormat::Other(source.format.id.clone()),
        ));
    }
    let metadata = fs::metadata(&source.path)?;
    if !metadata.is_file() {
        return Err(crate::mindmap_import::ImportError::InvalidDocument(
            "导入源必须是普通文件".to_string(),
        ));
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(crate::mindmap_import::ImportError::InvalidDocument(
            format!(
                "源文件超过 {} MiB 的导入上限",
                MAX_SOURCE_BYTES / (1024 * 1024)
            ),
        ));
    }
    Ok(source)
}

fn read_source(path: &Path) -> Result<(Vec<u8>, String), crate::mindmap_import::ImportError> {
    let bytes = fs::read(path)?;
    let digest = sha256(&bytes);
    Ok((bytes, digest))
}

fn extract_document(
    source: &ImportSourceInfo,
    bytes: &[u8],
    options: &ConversionOptions,
    strategies: ContentStrategies,
) -> Result<OutlineDocument, crate::mindmap_import::ImportError> {
    options.validate()?;
    let format = source_format(&source.format.id)?;
    let input = SourceInput {
        path: Some(&source.path),
        bytes,
        format: &format,
    };
    let mut document = match format {
        SourceFormat::Markdown => MarkdownAdapter.extract(input, options)?,
        SourceFormat::Text => TextAdapter::default().extract(input, options)?,
        SourceFormat::Docx => DocxAdapter::default().extract(input, options)?,
        SourceFormat::Html => HtmlAdapter.extract(input, options)?,
        SourceFormat::Opml => OpmlAdapter.extract(input, options)?,
        _ => {
            return Err(crate::mindmap_import::ImportError::UnsupportedFormat(
                format,
            ));
        }
    };
    apply_content_strategies(&mut document, strategies);
    document.stats.skipped_items = document
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning.code,
                ConversionWarningCode::ContentSkipped | ConversionWarningCode::EmptyInput
            )
        })
        .count();
    Ok(document)
}

fn source_format(id: &str) -> Result<SourceFormat, crate::mindmap_import::ImportError> {
    match id {
        "markdown" => Ok(SourceFormat::Markdown),
        "text" => Ok(SourceFormat::Text),
        "docx" => Ok(SourceFormat::Docx),
        "html" => Ok(SourceFormat::Html),
        "opml" => Ok(SourceFormat::Opml),
        other => Err(crate::mindmap_import::ImportError::UnsupportedFormat(
            SourceFormat::Other(other.to_string()),
        )),
    }
}

fn apply_content_strategies(document: &mut OutlineDocument, strategies: ContentStrategies) {
    if strategies.table == TableStrategy::RowsAsTopics {
        expand_table_rows(&mut document.children, &mut document.warnings);
        expand_document_tables(document);
    }
    if strategies.code == CodeStrategy::ParentNote {
        fold_children(&mut document.children, is_code_topic);
    }
    if strategies.image == ImageStrategy::ParentNote {
        fold_children(&mut document.children, is_image_topic);
    }
}

fn expand_table_rows(nodes: &mut [OutlineNode], warnings: &mut Vec<ConversionWarning>) {
    for node in nodes {
        expand_table_rows(&mut node.children, warnings);
        let tables = std::mem::take(&mut node.tables);
        for (table_index, table) in tables.into_iter().enumerate() {
            let fallback_source = node.source.clone();
            let table_topic = table_topic(table, table_index, fallback_source, warnings);
            node.children.push(table_topic);
        }
    }
}

fn expand_document_tables(document: &mut OutlineDocument) {
    let tables = std::mem::take(&mut document.tables);
    for (table_index, table) in tables.into_iter().enumerate() {
        let table_topic = table_topic(table, table_index, None, &mut document.warnings);
        document.children.push(table_topic);
    }
}

fn table_topic(
    table: crate::mindmap_import::OutlineTable,
    table_index: usize,
    fallback_source: Option<crate::mindmap_import::SourceLocation>,
    warnings: &mut Vec<ConversionWarning>,
) -> OutlineNode {
    let title = table
        .caption
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("表格 {}", table_index + 1));
    let source = table.source.clone().or(fallback_source);
    warnings.push(ConversionWarning::warning(
        ConversionWarningCode::TableFlattened,
        format!(
            "Table \"{title}\" was expanded into one topic per data row; header/caption context was retained in the table topic note"
        ),
        source.clone(),
    ));
    let mut table_topic = OutlineNode::content(
        title,
        crate::mindmap_import::ContentKind::Other("table".to_string()),
    );
    table_topic.source = source.clone();

    let mut context = Vec::new();
    if let Some(caption) = table
        .caption
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        context.push(format!("Caption: {caption}"));
    }
    if !table.headers.is_empty() {
        context.push(format!("Headers: {}", table.headers.join(" | ")));
    }
    if !context.is_empty() {
        table_topic
            .notes
            .push(crate::mindmap_import::OutlineNote::plain(
                context.join("\n"),
                source.clone(),
            ));
    }

    for (row_index, row) in table.rows.into_iter().enumerate() {
        let row_text = row.join(" | ");
        let title = if row_text.trim().is_empty() {
            format!("Row {}", row_index + 1)
        } else {
            row_text.clone()
        };
        let mut row_topic = OutlineNode::content(
            title,
            crate::mindmap_import::ContentKind::Other("table_row".to_string()),
        );
        row_topic.source = source.clone();
        if !row_text.trim().is_empty() {
            row_topic
                .notes
                .push(crate::mindmap_import::OutlineNote::plain(
                    row_text,
                    source.clone(),
                ));
        }
        table_topic.children.push(row_topic);
    }
    table_topic
}

fn fold_children(nodes: &mut [OutlineNode], predicate: fn(&OutlineNode) -> bool) {
    for node in nodes {
        fold_children(&mut node.children, predicate);
        let children = std::mem::take(&mut node.children);
        let mut retained = Vec::with_capacity(children.len());
        for child in children {
            if predicate(&child) {
                if !child.notes.is_empty() {
                    let mut notes = child.notes;
                    if notes.iter().all(|note| !note.text.contains(&child.title)) {
                        notes.insert(
                            0,
                            crate::mindmap_import::OutlineNote::plain(
                                child.title.clone(),
                                child.source.clone(),
                            ),
                        );
                    }
                    node.notes.extend(notes);
                } else {
                    node.notes.push(crate::mindmap_import::OutlineNote::plain(
                        child.title.clone(),
                        child.source.clone(),
                    ));
                }
                node.links.extend(child.links);
                node.attachments.extend(child.attachments);
                node.tables.extend(child.tables);
                // Nested children are retained as real topics. The common import
                // adapters create leaf code/image topics, but retaining descendants
                // prevents a strategy change from silently dropping user content.
                retained.extend(child.children);
            } else {
                retained.push(child);
            }
        }
        node.children = retained;
    }
}

fn is_code_topic(node: &OutlineNode) -> bool {
    matches!(
        node.kind,
        OutlineNodeKind::Content {
            kind: crate::mindmap_import::ContentKind::CodeBlock,
            ..
        }
    )
}

fn is_image_topic(node: &OutlineNode) -> bool {
    matches!(
        node.kind,
        OutlineNodeKind::Content {
            kind: crate::mindmap_import::ContentKind::Other(ref label),
            ..
        } if label == "image"
    )
}

#[derive(Debug)]
struct SampleTree {
    json: String,
    node_count: usize,
}

fn sample_tree(
    document: &OutlineDocument,
) -> Result<SampleTree, crate::mindmap_import::ImportError> {
    let mut sampled = 0usize;
    let mut root_children = Vec::new();
    for (index, child) in document.children.iter().enumerate() {
        if sampled >= MAX_PREVIEW_NODES {
            break;
        }
        root_children.push(sample_node(child, &format!("{index}"), 1, &mut sampled));
    }
    let root = json!({
        "id": "root",
        "title": truncate_preview_title(&document.title),
        "kind": "document",
        "noteCount": document.notes.len(),
        "structureConfidence": preview_structure_confidence(document),
        "inferredHeadingCount": original_format_count(document, "inferred_heading_count"),
        "ambiguousShortLineCount": original_format_count(document, "ambiguous_short_line_count"),
        "children": root_children,
    });
    Ok(SampleTree {
        json: serde_json::to_string(&root)?,
        node_count: sampled,
    })
}

fn sample_node(node: &OutlineNode, path: &str, depth: usize, sampled: &mut usize) -> Value {
    *sampled += 1;
    let mut children = Vec::new();
    if depth < 5 && *sampled < MAX_PREVIEW_NODES {
        for (index, child) in node.children.iter().enumerate() {
            if *sampled >= MAX_PREVIEW_NODES {
                break;
            }
            children.push(sample_node(
                child,
                &format!("{path}.{index}"),
                depth + 1,
                sampled,
            ));
        }
    }
    json!({
        "id": path,
        "title": truncate_preview_title(&node.title),
        "kind": node_kind_label(node),
        "noteCount": node.notes.len(),
        "childCount": node.children.len(),
        "children": children,
    })
}

fn node_kind_label(node: &OutlineNode) -> &'static str {
    match &node.kind {
        OutlineNodeKind::Structure { .. } => "structure",
        OutlineNodeKind::Content { kind, .. } => match kind {
            crate::mindmap_import::ContentKind::Paragraph => "paragraph",
            crate::mindmap_import::ContentKind::Sentence => "sentence",
            crate::mindmap_import::ContentKind::Quote => "quote",
            crate::mindmap_import::ContentKind::CodeBlock => "code",
            crate::mindmap_import::ContentKind::Task => "task",
            crate::mindmap_import::ContentKind::Other(value) if value == "image" => "image",
            crate::mindmap_import::ContentKind::Other(value) if value == "table" => "table",
            crate::mindmap_import::ContentKind::Other(_) => "content",
        },
    }
}

fn truncate_preview_title(title: &str) -> String {
    let mut characters = title.trim().chars();
    let truncated: String = characters.by_ref().take(MAX_PREVIEW_TITLE_CHARS).collect();
    if characters.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn preview_structure_confidence(document: &OutlineDocument) -> String {
    document
        .source
        .original_format
        .get("structure_confidence")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .filter(|value| matches!(value.as_str(), "low" | "medium" | "high"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn original_format_count(document: &OutlineDocument, key: &str) -> usize {
    document
        .source
        .original_format
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn validated_output_path(
    source_path: &Path,
    output_path: &Path,
    overwrite_existing: bool,
) -> Result<PathBuf, crate::mindmap_import::ImportError> {
    let output = normalized_path(output_path);
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !extension.eq_ignore_ascii_case("xmind") {
        return Err(crate::mindmap_import::ImportError::InvalidOptions(
            "输出文件名必须以 .xmind 结尾".to_string(),
        ));
    }
    let source_canonical = fs::canonicalize(source_path)?;
    let output_canonical = if output.exists() {
        fs::canonicalize(&output)?
    } else {
        let parent = output.parent().ok_or_else(|| {
            crate::mindmap_import::ImportError::InvalidOptions("输出路径缺少父目录".to_string())
        })?;
        let parent = fs::canonicalize(parent).map_err(|_| {
            crate::mindmap_import::ImportError::InvalidOptions("输出目录不存在".to_string())
        })?;
        parent.join(output.file_name().ok_or_else(|| {
            crate::mindmap_import::ImportError::InvalidOptions("输出文件名不能为空".to_string())
        })?)
    };
    if output_canonical == source_canonical {
        return Err(crate::mindmap_import::ImportError::InvalidOptions(
            "输出路径不能覆盖源文件".to_string(),
        ));
    }
    if output.exists() && !overwrite_existing {
        return Err(crate::mindmap_import::ImportError::InvalidOptions(
            "输出文件已存在；请确认允许覆盖".to_string(),
        ));
    }
    Ok(output)
}

fn normalized_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn unique_output_path(source_path: &Path) -> PathBuf {
    let source = normalized_path(source_path);
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Mindmap");
    let first = parent.join(format!("{stem}.xmind"));
    if !first.exists() {
        return first;
    }
    for index in 2..10_000 {
        let candidate = parent.join(format!("{stem} {index}.xmind"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} Mindmap.xmind"))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
