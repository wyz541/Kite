//! Deterministic modern `.xmind` writer for the shared outline model.

use std::io::{Cursor, Write};
use std::path::Path;

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde_json::{Map, Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::mindmap::XmindPackage;
use crate::mindmap_import::model::{
    ConversionOptions, ConversionReport, ConversionWarning, ConversionWarningCode, ImportError,
    OutlineAttachment, OutlineDocument, OutlineLink, OutlineNode, OutlineNote, OutlineTable,
    SourceLocation, TaskState, stable_id, truncate_text,
};

pub const CONTENT_ENTRY: &str = "content.json";
pub const METADATA_ENTRY: &str = "metadata.json";
pub const MANIFEST_ENTRY: &str = "manifest.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmindWriteResult {
    pub bytes: Vec<u8>,
    pub report: ConversionReport,
}

pub fn write_xmind_bytes(
    document: &OutlineDocument,
    options: &ConversionOptions,
) -> Result<XmindWriteResult, ImportError> {
    options.validate()?;
    if document.node_count() > options.max_nodes {
        return Err(ImportError::LimitExceeded {
            limit: "outline node count",
            maximum: options.max_nodes,
        });
    }
    if document.max_depth() > usize::from(options.max_depth) {
        return Err(ImportError::LimitExceeded {
            limit: "outline depth",
            maximum: usize::from(options.max_depth),
        });
    }

    let (document_title, _) = truncate_text(&document.title, options.max_title_chars);
    let document_title = if document_title.is_empty() {
        "Untitled document".to_string()
    } else {
        document_title
    };
    let source_key = document_source_key(document);
    let sheet_id = stable_id(&format!("sheet|{source_key}|{document_title}"));
    let root_id = stable_id(&format!("root|{source_key}|{document_title}"));
    let mut root = topic_object(
        &root_id,
        &document_title,
        options.max_title_chars,
        &document.links,
    );
    root.insert(
        "notes".to_string(),
        Value::Object(topic_notes_object(
            &document.notes,
            &document.links,
            &document.attachments,
            &document.tables,
            Some(provenance_note(document, options)),
            None,
        )),
    );

    let children = document
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            node_to_json(
                child,
                &source_key,
                &format!("{index}"),
                options.max_title_chars,
            )
        })
        .collect::<Vec<_>>();
    if !children.is_empty() {
        root.insert("children".to_string(), json!({ "attached": children }));
    }

    let content = json!([
        {
            "id": sheet_id,
            "class": "sheet",
            "title": document_title,
            "rootTopic": Value::Object(root),
        }
    ]);
    let metadata = json!({
        "creator": {
            "name": "Kite",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "activeSheetId": sheet_id,
    });
    let manifest = json!({
        "file-entries": {
            CONTENT_ENTRY: {},
            METADATA_ENTRY: {},
        }
    });

    let bytes = build_zip(&content, &metadata, &manifest)?;
    // Re-open with the production container reader so the writer cannot return
    // a package that bypasses the same validation used by the editor.
    XmindPackage::from_bytes(bytes.clone())?;

    let mut report = document.report(options);
    report.warnings.extend(degradation_warnings(document));
    Ok(XmindWriteResult { bytes, report })
}

pub fn write_xmind_file(
    document: &OutlineDocument,
    options: &ConversionOptions,
    path: &Path,
) -> Result<XmindWriteResult, ImportError> {
    let result = write_xmind_bytes(document, options)?;
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| file.write_all(&result.bytes))
        .map_err(|error| ImportError::Write(error.to_string()))?;
    Ok(result)
}

fn node_to_json(node: &OutlineNode, source_key: &str, path: &str, max_title_chars: usize) -> Value {
    let id = stable_id(&format!("topic|{source_key}|{path}|{}", node.title));
    let mut object = topic_object(&id, &node.title, max_title_chars, &node.links);

    if has_topic_notes(node) {
        object.insert(
            "notes".to_string(),
            Value::Object(topic_notes_object(
                &node.notes,
                &node.links,
                &node.attachments,
                &node.tables,
                None,
                node.task.clone(),
            )),
        );
    }

    let children = node
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            node_to_json(
                child,
                source_key,
                &format!("{path}.{index}"),
                max_title_chars,
            )
        })
        .collect::<Vec<_>>();
    if !children.is_empty() {
        object.insert("children".to_string(), json!({ "attached": children }));
    }

    Value::Object(object)
}

fn topic_object(
    id: &str,
    title: &str,
    max_title_chars: usize,
    links: &[OutlineLink],
) -> Map<String, Value> {
    let (safe_title, _) = truncate_text(title, max_title_chars);
    let title = if safe_title.is_empty() {
        "Untitled topic".to_string()
    } else {
        safe_title
    };

    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(id.to_string()));
    object.insert("class".to_string(), Value::String("topic".to_string()));
    object.insert("title".to_string(), Value::String(title));
    if let Some(link) = links.first() {
        object.insert("href".to_string(), Value::String(link.target.clone()));
    }
    object
}

fn has_topic_notes(node: &OutlineNode) -> bool {
    node.notes.iter().any(|note| !note.text.trim().is_empty())
        || !node.links.is_empty()
        || !node.attachments.is_empty()
        || !node.tables.is_empty()
        || node.task.is_some()
}

fn topic_notes_object(
    notes: &[OutlineNote],
    links: &[OutlineLink],
    attachments: &[OutlineAttachment],
    tables: &[OutlineTable],
    suffix: Option<String>,
    task: Option<TaskState>,
) -> Map<String, Value> {
    let mut sections = Vec::<String>::new();
    for note in notes {
        if note.text.trim().is_empty() {
            continue;
        }
        if let Some(label) = note.label.as_deref() {
            sections.push(format!("{label}:\n{}", note.text));
        } else {
            sections.push(note.text.clone());
        }
    }

    for link in links {
        let label = link.label.as_deref().unwrap_or(&link.target);
        sections.push(format!("Link: [{label}]({})", link.target));
    }

    for attachment in attachments {
        let mut item = format!("Attachment: {}", attachment.name);
        if let Some(media_type) = attachment.media_type.as_deref() {
            item.push_str(&format!(" ({media_type})"));
        }
        if let Some(uri) = attachment.uri.as_deref() {
            item.push_str(&format!("\n{uri}"));
        }
        if attachment.data.is_some() {
            item.push_str("\nEmbedded attachment bytes are retained in the source model.");
        }
        sections.push(item);
    }

    for table in tables {
        sections.push(table_as_markdown(table));
    }

    if let Some(task) = task {
        sections.push(format!(
            "Task status: {}",
            match task {
                TaskState::Todo => "todo",
                TaskState::Done => "done",
            }
        ));
    }

    if let Some(suffix) = suffix.filter(|value| !value.trim().is_empty()) {
        sections.push(suffix);
    }

    let mut plain = Map::new();
    plain.insert("content".to_string(), Value::String(sections.join("\n\n")));
    let mut notes = Map::new();
    notes.insert("plain".to_string(), Value::Object(plain));
    notes
}

fn table_as_markdown(table: &OutlineTable) -> String {
    let mut lines = Vec::new();
    if let Some(caption) = table
        .caption
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(caption.to_string());
    }
    if !table.headers.is_empty() {
        lines.push(format!("| {} |", table.headers.join(" | ")));
        lines.push(format!(
            "| {} |",
            table
                .headers
                .iter()
                .map(|_| "---")
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    for row in &table.rows {
        lines.push(format!("| {} |", row.join(" | ")));
    }
    lines.join("\n")
}

fn provenance_note(document: &OutlineDocument, options: &ConversionOptions) -> String {
    let mut lines = vec![
        format!("Source format: {}", document.source.format),
        format!(
            "Adapter: {} {}",
            document.source.adapter, document.source.adapter_version
        ),
        format!("Conversion preset: {}", preset_name(&options.preset)),
        format!("Outline schema: {}", document.schema_version),
        format!(
            "Import pipeline: {}",
            crate::mindmap_import::IMPORT_PIPELINE_VERSION
        ),
    ];
    if !document.source.source_sha256.is_empty() {
        lines.push(format!("Source SHA-256: {}", document.source.source_sha256));
    }
    if options.include_source_path
        && let Some(path) = document.source.source_path.as_deref()
    {
        lines.push(format!("Source path: {path}"));
    }
    lines.join("\n")
}

fn preset_name(preset: &crate::mindmap_import::model::ConversionPreset) -> &'static str {
    use crate::mindmap_import::model::ConversionPreset;

    match preset {
        ConversionPreset::StructureFirst => "structure-first",
        ConversionPreset::ExpandedContent => "expanded-content",
        ConversionPreset::StrictOutline => "strict-outline",
    }
}

fn document_source_key(document: &OutlineDocument) -> String {
    if !document.source.source_sha256.is_empty() {
        document.source.source_sha256.clone()
    } else if let Some(path) = document.source.source_path.as_deref() {
        path.to_string()
    } else {
        format!("{}|{}", document.title, document.source.adapter)
    }
}

fn degradation_warnings(document: &OutlineDocument) -> Vec<ConversionWarning> {
    let mut warnings = Vec::new();
    for (table, source) in document_tables(document) {
        let detail = table
            .caption
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("untitled table");
        warnings.push(ConversionWarning::warning(
            ConversionWarningCode::TableFlattened,
            format!("Table \"{detail}\" was flattened into topic notes"),
            source,
        ));
    }
    for (attachment, source) in document_attachments(document) {
        warnings.push(ConversionWarning::warning(
            ConversionWarningCode::AttachmentReferenceOnly,
            format!(
                "Attachment \"{}\" is represented as a note reference in this import slice",
                attachment.name
            ),
            source,
        ));
    }
    warnings
}

fn document_tables(document: &OutlineDocument) -> Vec<(&OutlineTable, Option<SourceLocation>)> {
    let mut tables = document
        .tables
        .iter()
        .map(|table| (table, table.source.clone()))
        .collect::<Vec<_>>();
    for node in &document.children {
        collect_node_tables(node, &mut tables);
    }
    tables
}

fn collect_node_tables<'a>(
    node: &'a OutlineNode,
    output: &mut Vec<(&'a OutlineTable, Option<SourceLocation>)>,
) {
    for table in &node.tables {
        output.push((table, table.source.clone().or_else(|| node.source.clone())));
    }
    for child in &node.children {
        collect_node_tables(child, output);
    }
}

fn document_attachments(
    document: &OutlineDocument,
) -> Vec<(&OutlineAttachment, Option<SourceLocation>)> {
    let mut attachments = document
        .attachments
        .iter()
        .map(|attachment| (attachment, attachment.source.clone()))
        .collect::<Vec<_>>();
    for node in &document.children {
        collect_node_attachments(node, &mut attachments);
    }
    attachments
}

fn collect_node_attachments<'a>(
    node: &'a OutlineNode,
    output: &mut Vec<(&'a OutlineAttachment, Option<SourceLocation>)>,
) {
    for attachment in &node.attachments {
        output.push((
            attachment,
            attachment.source.clone().or_else(|| node.source.clone()),
        ));
    }
    for child in &node.children {
        collect_node_attachments(child, output);
    }
}

fn build_zip(content: &Value, metadata: &Value, manifest: &Value) -> Result<Vec<u8>, ImportError> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut cursor);
        let zip_time = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
            .map_err(|error| ImportError::Write(error.to_string()))?;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip_time)
            .unix_permissions(0o644);
        for (name, value) in [
            (CONTENT_ENTRY, content),
            (METADATA_ENTRY, metadata),
            (MANIFEST_ENTRY, manifest),
        ] {
            writer.start_file(name, options)?;
            writer.write_all(&serde_json::to_vec(value)?)?;
        }
        writer.finish()?;
    }
    Ok(cursor.into_inner())
}
