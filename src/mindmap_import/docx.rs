//! DOCX (Office Open XML) adapter for the shared outline import pipeline.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use roxmltree::{Document as XmlDocument, Node};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::mindmap_import::model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode,
    ImportError, LinkKind, OutlineAdapter, OutlineAttachment, OutlineDocument, OutlineLink,
    OutlineNode, OutlineNote, OutlineTable, SourceFormat, SourceInput, SourceLocation,
    SourceProvenance, StructureKind, truncate_text,
};
use crate::mindmap_import::segment::{ParagraphExpansionReport, add_paragraph_as_content_nodes};
use crate::mindmap_import::tree_builder::OutlineTreeBuilder;

const ADAPTER_NAME: &str = "docx-ooxml";
const ADAPTER_VERSION: &str = "1.0.0";
const MAX_DOCX_BYTES: usize = 256 * 1024 * 1024;
const MAX_DOCX_UNCOMPRESSED_BYTES: usize = 768 * 1024 * 1024;
const MAX_DOCX_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DOCX_MEDIA_BYTES: u64 = 128 * 1024 * 1024;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const V_NS: &str = "urn:schemas-microsoft-com:vml";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocxAdapterOptions {
    /// Disabled by default. When enabled, bold/large paragraphs are treated as
    /// headings only when the document has no structural Heading/outline level.
    pub heading_heuristics: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DocxAdapter {
    options: DocxAdapterOptions,
}

impl DocxAdapter {
    pub fn new(options: DocxAdapterOptions) -> Self {
        Self { options }
    }

    pub fn options(&self) -> &DocxAdapterOptions {
        &self.options
    }
}

impl OutlineAdapter for DocxAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            name: ADAPTER_NAME.to_string(),
            version: ADAPTER_VERSION.to_string(),
            formats: vec![SourceFormat::Docx],
        }
    }

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError> {
        if !matches!(source.format, SourceFormat::Docx) {
            return Err(ImportError::UnsupportedFormat(source.format.clone()));
        }
        options.validate()?;
        extract_docx(source, options, &self.options)
    }
}

#[derive(Debug)]
struct DocxArchive {
    entries: BTreeMap<String, Vec<u8>>,
}

impl DocxArchive {
    fn open(bytes: &[u8]) -> Result<Self, ImportError> {
        if bytes.len() > MAX_DOCX_BYTES {
            return Err(ImportError::InvalidDocument(format!(
                "DOCX exceeds the {} byte import limit",
                MAX_DOCX_BYTES
            )));
        }
        let mut archive = ZipArchive::new(Cursor::new(bytes))?;
        let mut entries = BTreeMap::new();
        let mut total_size = 0_usize;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            if name.starts_with('/') || name.split('/').any(|part| part == "..") {
                return Err(ImportError::InvalidDocument(format!(
                    "DOCX contains an unsafe ZIP path: {name}"
                )));
            }
            let declared_size = entry.size();
            let size = usize::try_from(declared_size).unwrap_or(usize::MAX);
            total_size = total_size
                .checked_add(size)
                .ok_or_else(|| ImportError::InvalidDocument("DOCX size overflow".to_string()))?;
            if total_size > MAX_DOCX_UNCOMPRESSED_BYTES {
                return Err(ImportError::InvalidDocument(format!(
                    "DOCX expands beyond the {} byte limit",
                    MAX_DOCX_UNCOMPRESSED_BYTES
                )));
            }
            let mut data = Vec::with_capacity(size.min(1024 * 1024));
            entry
                .by_ref()
                .take(declared_size.saturating_add(1))
                .read_to_end(&mut data)?;
            if data.len() as u64 > declared_size {
                return Err(ImportError::InvalidDocument(format!(
                    "DOCX ZIP entry {name} exceeds its declared size"
                )));
            }
            if entries.insert(name.clone(), data).is_some() {
                return Err(ImportError::InvalidDocument(format!(
                    "DOCX contains a duplicate ZIP entry: {name}"
                )));
            }
        }
        Ok(Self { entries })
    }

    fn bytes(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(Vec::as_slice)
    }

    fn xml(&self, name: &str) -> Result<Option<XmlDocument<'_>>, ImportError> {
        let Some(bytes) = self.bytes(name) else {
            return Ok(None);
        };
        if bytes.len() as u64 > MAX_DOCX_XML_BYTES {
            return Err(ImportError::InvalidDocument(format!(
                "DOCX XML entry {name} exceeds the size limit"
            )));
        }
        let text = std::str::from_utf8(bytes).map_err(|error| {
            ImportError::InvalidDocument(format!("DOCX XML entry {name} is not UTF-8: {error}"))
        })?;
        XmlDocument::parse(text).map(Some).map_err(|error| {
            ImportError::InvalidDocument(format!("invalid DOCX XML entry {name}: {error}"))
        })
    }

    fn media(&self, name: &str) -> Result<Option<Vec<u8>>, ImportError> {
        let Some(bytes) = self.bytes(name) else {
            return Ok(None);
        };
        if bytes.len() as u64 > MAX_DOCX_MEDIA_BYTES {
            return Err(ImportError::InvalidDocument(format!(
                "DOCX media entry {name} exceeds the size limit"
            )));
        }
        Ok(Some(bytes.to_vec()))
    }
}

#[derive(Clone, Debug)]
struct Relationship {
    target: String,
    external: bool,
}

#[derive(Clone, Debug, Default)]
struct Relationships {
    by_id: BTreeMap<String, Relationship>,
}

impl Relationships {
    fn parse(archive: &DocxArchive, entry: &str) -> Result<Self, ImportError> {
        let Some(document) = archive.xml(entry)? else {
            return Ok(Self::default());
        };
        let mut by_id = BTreeMap::new();
        for relation in document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "Relationship")
        {
            let Some(id) = attribute_local(relation, "Id") else {
                continue;
            };
            let Some(target) = attribute_local(relation, "Target") else {
                continue;
            };
            let external = attribute_local(relation, "TargetMode")
                .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
            by_id.insert(
                id.to_string(),
                Relationship {
                    target: target.to_string(),
                    external,
                },
            );
        }
        Ok(Self { by_id })
    }

    fn get(&self, id: &str) -> Option<&Relationship> {
        self.by_id.get(id)
    }
}

#[derive(Clone, Debug, Default)]
struct StyleDefinition {
    name: Option<String>,
    based_on: Option<String>,
    outline_level: Option<u8>,
}

#[derive(Clone, Debug, Default)]
struct StyleSheet {
    styles: BTreeMap<String, StyleDefinition>,
}

#[derive(Clone, Debug, Default)]
struct ResolvedStyle {
    name: Option<String>,
    outline_level: Option<u8>,
    heading_level: Option<u8>,
    title_like: bool,
}

impl StyleSheet {
    fn parse(archive: &DocxArchive) -> Result<Self, ImportError> {
        let Some(document) = archive.xml("word/styles.xml")? else {
            return Ok(Self::default());
        };
        let mut styles = BTreeMap::new();
        for style in document
            .descendants()
            .filter(|node| tag_is(*node, W_NS, "style"))
        {
            let Some(style_id) = attribute_local(style, "styleId") else {
                continue;
            };
            let name = style
                .children()
                .find(|child| tag_is(*child, W_NS, "name"))
                .and_then(|node| attribute_local(node, "val"))
                .map(ToString::to_string);
            let based_on = style
                .children()
                .find(|child| tag_is(*child, W_NS, "basedOn"))
                .and_then(|node| attribute_local(node, "val"))
                .map(ToString::to_string);
            let outline_level = paragraph_outline_level(style).map(|level| level.saturating_sub(1));
            styles.insert(
                style_id.to_string(),
                StyleDefinition {
                    name,
                    based_on,
                    outline_level,
                },
            );
        }
        Ok(Self { styles })
    }

    fn resolve(&self, style_id: Option<&str>) -> ResolvedStyle {
        let Some(style_id) = style_id else {
            return ResolvedStyle::default();
        };
        let mut current = Some(style_id);
        let mut visited = BTreeSet::new();
        let mut name = None;
        let mut outline_level = None;
        let mut heading_level = None;
        let mut title_like = false;
        for _ in 0..24 {
            let Some(id) = current else {
                break;
            };
            if !visited.insert(id.to_string()) {
                break;
            }
            let Some(definition) = self.styles.get(id) else {
                break;
            };
            if name.is_none() {
                name.clone_from(&definition.name);
            }
            if outline_level.is_none() {
                outline_level = definition.outline_level;
            }
            if heading_level.is_none() {
                heading_level =
                    heading_level_from_style(id, definition.name.as_deref().unwrap_or(id));
            }
            title_like |= is_title_style(id, definition.name.as_deref().unwrap_or(id));
            current = definition.based_on.as_deref();
        }
        let resolved_name = name.or_else(|| Some(style_id.to_string()));
        ResolvedStyle {
            heading_level: heading_level.or_else(|| {
                resolved_name
                    .as_deref()
                    .and_then(|name| heading_level_from_style(style_id, name))
            }),
            title_like,
            name: resolved_name,
            outline_level,
        }
    }

    fn known_or_heading_style(&self, style_id: &str) -> bool {
        let resolved = self.resolve(Some(style_id));
        let name = resolved.name.as_deref().unwrap_or(style_id);
        if resolved.heading_level.is_some()
            || resolved.title_like
            || resolved.outline_level.is_some()
        {
            return true;
        }
        let normalized = normalize_style_name(name);
        matches!(
            normalized.as_str(),
            "normal"
                | "bodytext"
                | "listparagraph"
                | "quote"
                | "intensequote"
                | "caption"
                | "subtitle"
                | "bibliography"
                | "footnotetext"
                | "endnotetext"
                | "header"
                | "footer"
                | "tocheading"
                | "toc1"
                | "toc2"
                | "toc3"
                | "正文"
                | "列表段落"
                | "引用"
                | "标题"
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct ListInfo {
    num_id: u32,
    level: u8,
}

#[derive(Clone, Debug, Default)]
struct Numbering {
    ordered: BTreeMap<(u32, u8), bool>,
    num_to_abstract: BTreeMap<u32, u32>,
    abstract_formats: BTreeMap<(u32, u8), String>,
}

impl Numbering {
    fn parse(archive: &DocxArchive) -> Result<Self, ImportError> {
        let Some(document) = archive.xml("word/numbering.xml")? else {
            return Ok(Self::default());
        };
        let mut ordered = BTreeMap::new();
        let mut num_to_abstract = BTreeMap::new();
        let mut abstract_formats = BTreeMap::new();
        for abstract_num in document
            .descendants()
            .filter(|node| tag_is(*node, W_NS, "abstractNum"))
        {
            let Some(abstract_id) = attribute_local(abstract_num, "abstractNumId")
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            for level in abstract_num
                .children()
                .filter(|node| tag_is(*node, W_NS, "lvl"))
            {
                let ilvl = attribute_local(level, "ilvl")
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0);
                let format = level
                    .children()
                    .find(|node| tag_is(*node, W_NS, "numFmt"))
                    .and_then(|node| attribute_local(node, "val"))
                    .unwrap_or("decimal")
                    .to_string();
                abstract_formats.insert((abstract_id, ilvl), format);
            }
        }
        for num in document
            .descendants()
            .filter(|node| tag_is(*node, W_NS, "num"))
        {
            let Some(num_id) =
                attribute_local(num, "numId").and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            let Some(abstract_id) = num
                .children()
                .find(|node| tag_is(*node, W_NS, "abstractNumId"))
                .and_then(|node| attribute_local(node, "val"))
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            num_to_abstract.insert(num_id, abstract_id);
        }
        for (&(abstract_id, ilvl), format) in &abstract_formats {
            if let Some((&num_id, _)) = num_to_abstract
                .iter()
                .find(|(_, candidate)| **candidate == abstract_id)
            {
                ordered.insert((num_id, ilvl), format != "bullet");
            }
        }
        Ok(Self {
            ordered,
            num_to_abstract,
            abstract_formats,
        })
    }

    fn is_ordered(&self, info: ListInfo) -> bool {
        self.ordered
            .get(&(info.num_id, info.level))
            .copied()
            .or_else(|| {
                let abstract_id = self.num_to_abstract.get(&info.num_id)?;
                self.abstract_formats
                    .get(&(*abstract_id, info.level))
                    .map(|format| format != "bullet")
            })
            .unwrap_or(true)
    }
}

#[derive(Clone, Debug, Default)]
struct NoteRecord {
    author: Option<String>,
    date: Option<String>,
    text: String,
}

#[derive(Clone, Debug, Default)]
struct ParagraphAssets {
    links: Vec<OutlineLink>,
    attachments: Vec<OutlineAttachment>,
    warnings: Vec<ConversionWarning>,
    footnote_ids: Vec<String>,
    endnote_ids: Vec<String>,
    comment_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct DocumentState {
    paragraph_count: usize,
    heading_count: usize,
    table_count: usize,
    tracked_change_count: usize,
    unknown_styles: BTreeSet<String>,
    used_heuristic_heading: bool,
}

fn extract_docx(
    source: SourceInput<'_>,
    conversion: &ConversionOptions,
    adapter_options: &DocxAdapterOptions,
) -> Result<OutlineDocument, ImportError> {
    let archive = DocxArchive::open(source.bytes)?;
    let document_xml = archive.xml("word/document.xml")?.ok_or_else(|| {
        ImportError::InvalidDocument("DOCX is missing word/document.xml".to_string())
    })?;
    let body = document_xml
        .descendants()
        .find(|node| tag_is(*node, W_NS, "body"))
        .ok_or_else(|| {
            ImportError::InvalidDocument("DOCX word/document.xml has no body".to_string())
        })?;
    let styles = StyleSheet::parse(&archive)?;
    let numbering = Numbering::parse(&archive)?;
    let relationships = Relationships::parse(&archive, "word/_rels/document.xml.rels")?;
    let footnotes = parse_notes(&archive, "word/footnotes.xml", "footnote")?;
    let endnotes = parse_notes(&archive, "word/endnotes.xml", "endnote")?;
    let comments = parse_comments(&archive)?;
    let core_title = parse_core_title(&archive)?;
    let fallback_title = source
        .path
        .and_then(Path::file_stem)
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Untitled DOCX")
        .to_string();
    let has_structural_heading = document_has_structural_heading(body, &styles);
    let mut state = DocumentState::default();
    count_tracked_changes(body, &mut state);

    let mut provenance = SourceProvenance::new(SourceFormat::Docx, ADAPTER_NAME, ADAPTER_VERSION);
    provenance.source_path = source.path.and_then(Path::to_str).map(ToString::to_string);
    provenance.source_sha256 = format!("{:x}", Sha256::digest(source.bytes));
    provenance.original_format.insert(
        "heading_heuristics_enabled".to_string(),
        json!(adapter_options.heading_heuristics),
    );
    provenance.original_format.insert(
        "structural_headings_present".to_string(),
        json!(has_structural_heading),
    );
    provenance
        .original_format
        .insert("footnote_count".to_string(), json!(footnotes.len()));
    provenance
        .original_format
        .insert("endnote_count".to_string(), json!(endnotes.len()));
    provenance
        .original_format
        .insert("comment_count".to_string(), json!(comments.len()));

    let mut document =
        OutlineDocument::new(core_title.as_deref().unwrap_or(&fallback_title), provenance);
    let mut builder = OutlineTreeBuilder::new();
    let mut used_title_style = false;
    let mut referenced_footnotes = BTreeSet::new();
    let mut referenced_endnotes = BTreeSet::new();
    let mut referenced_comments = BTreeSet::new();
    let mut paragraph_index = 0_usize;

    {
        let mut reference_notes = ReferenceNotes {
            footnotes: &footnotes,
            endnotes: &endnotes,
            comments: &comments,
            referenced_footnotes: &mut referenced_footnotes,
            referenced_endnotes: &mut referenced_endnotes,
            referenced_comments: &mut referenced_comments,
        };
        process_block_children(
            body,
            &archive,
            &styles,
            &numbering,
            &relationships,
            &mut document,
            &mut builder,
            &mut state,
            conversion,
            adapter_options,
            has_structural_heading,
            core_title.is_some(),
            &mut used_title_style,
            &mut paragraph_index,
            &mut reference_notes,
        )?;
    }

    document.children = builder.finish();
    append_unreferenced_notes(
        &mut document,
        &footnotes,
        &endnotes,
        &comments,
        &referenced_footnotes,
        &referenced_endnotes,
        &referenced_comments,
    );
    add_docx_warnings(&mut document, &state, &footnotes, &endnotes, &comments);
    document
        .source
        .original_format
        .insert("paragraph_count".to_string(), json!(state.paragraph_count));
    document
        .source
        .original_format
        .insert("heading_count".to_string(), json!(state.heading_count));
    document
        .source
        .original_format
        .insert("table_count".to_string(), json!(state.table_count));
    document.source.original_format.insert(
        "tracked_change_count".to_string(),
        json!(state.tracked_change_count),
    );
    document.source.original_format.insert(
        "heuristic_heading_used".to_string(),
        json!(state.used_heuristic_heading),
    );

    if document.node_count() > conversion.max_nodes {
        return Err(ImportError::LimitExceeded {
            limit: "outline node count",
            maximum: conversion.max_nodes,
        });
    }
    if document.max_depth() > usize::from(conversion.max_depth) {
        return Err(ImportError::LimitExceeded {
            limit: "outline depth",
            maximum: usize::from(conversion.max_depth),
        });
    }
    if document.node_count() == 0 {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::EmptyInput,
            "DOCX contained no convertible body text",
            None,
        ));
    }
    Ok(document)
}

#[allow(clippy::too_many_arguments)]
fn process_block_children(
    parent: Node<'_, '_>,
    archive: &DocxArchive,
    styles: &StyleSheet,
    numbering: &Numbering,
    relationships: &Relationships,
    document: &mut OutlineDocument,
    builder: &mut OutlineTreeBuilder,
    state: &mut DocumentState,
    conversion: &ConversionOptions,
    adapter_options: &DocxAdapterOptions,
    has_structural_heading: bool,
    core_title_present: bool,
    used_title_style: &mut bool,
    paragraph_index: &mut usize,
    reference_notes: &mut ReferenceNotes<'_>,
) -> Result<(), ImportError> {
    for child in parent.children().filter(Node::is_element) {
        if tag_is(child, W_NS, "p") {
            process_paragraph(
                child,
                archive,
                styles,
                numbering,
                relationships,
                document,
                builder,
                state,
                conversion,
                adapter_options,
                has_structural_heading,
                core_title_present,
                used_title_style,
                paragraph_index,
                reference_notes,
            )?;
        } else if tag_is(child, W_NS, "tbl") {
            process_table(
                child,
                archive,
                relationships,
                document,
                builder,
                state,
                paragraph_index,
                reference_notes,
            )?;
        } else if tag_is(child, W_NS, "sdt")
            || tag_is(child, W_NS, "customXml")
            || tag_is(child, W_NS, "ins")
        {
            process_block_children(
                child,
                archive,
                styles,
                numbering,
                relationships,
                document,
                builder,
                state,
                conversion,
                adapter_options,
                has_structural_heading,
                core_title_present,
                used_title_style,
                paragraph_index,
                reference_notes,
            )?;
        } else if !tag_is(child, W_NS, "sectPr")
            && (child
                .descendants()
                .any(|node| tag_is(node, W_NS, "drawing"))
                || child.descendants().any(|node| tag_is(node, W_NS, "object")))
        {
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                "A non-paragraph DOCX drawing/object was flattened or skipped",
                Some(paragraph_source(child, *paragraph_index)),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_paragraph(
    paragraph: Node<'_, '_>,
    archive: &DocxArchive,
    styles: &StyleSheet,
    numbering: &Numbering,
    relationships: &Relationships,
    document: &mut OutlineDocument,
    builder: &mut OutlineTreeBuilder,
    state: &mut DocumentState,
    conversion: &ConversionOptions,
    adapter_options: &DocxAdapterOptions,
    has_structural_heading: bool,
    core_title_present: bool,
    used_title_style: &mut bool,
    paragraph_index: &mut usize,
    reference_notes: &mut ReferenceNotes<'_>,
) -> Result<(), ImportError> {
    *paragraph_index += 1;
    state.paragraph_count += 1;
    let source = paragraph_source(paragraph, *paragraph_index);
    let text = paragraph_text(paragraph);
    let style_id = paragraph_style_id(paragraph);
    let resolved_style = styles.resolve(style_id);
    let direct_outline = paragraph_outline_level(paragraph);
    let style_heading = resolved_style.heading_level;
    let heading_level = direct_outline
        .or(resolved_style
            .outline_level
            .map(|level| level.saturating_add(1)))
        .or(style_heading)
        .map(|level| level.clamp(1, conversion.max_depth));
    let title_style = resolved_style.title_like;
    let list = paragraph_list_info(paragraph);
    let quote_style = resolved_style.name.as_deref().is_some_and(|name| {
        let normalized = normalize_style_name(name);
        normalized.contains("quote") || normalized == "引用"
    });
    if let Some(style_id) = style_id
        && !styles.known_or_heading_style(style_id)
        && list.is_none()
        && heading_level.is_none()
        && !title_style
    {
        state.unknown_styles.insert(style_id.to_string());
    }

    let mut assets = collect_paragraph_assets(paragraph, archive, relationships, &source)?;
    let heuristic_heading = if heading_level.is_none()
        && !title_style
        && list.is_none()
        && adapter_options.heading_heuristics
        && !has_structural_heading
    {
        heuristic_heading_level(paragraph, &text)
    } else {
        None
    };
    if heuristic_heading.is_some() {
        state.used_heuristic_heading = true;
    }
    let effective_heading = heading_level.or(heuristic_heading);

    let target_path = if title_style
        && !core_title_present
        && !*used_title_style
        && !text.is_empty()
    {
        *used_title_style = true;
        document.title = text.clone();
        document.notes.push(OutlineNote::with_label(
            &text,
            "Word Title style",
            Some(source.clone()),
        ));
        None
    } else if let Some(level) = effective_heading {
        state.heading_count += 1;
        let (title, truncated) = truncate_text(&text, conversion.max_title_chars);
        if truncated {
            document.stats.truncated_titles += 1;
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::TitleTruncated,
                "DOCX heading title was truncated",
                Some(source.clone()),
            ));
        }
        let mut node = OutlineNode::structure(
            if title.is_empty() {
                "Untitled heading"
            } else {
                &title
            },
            StructureKind::Heading { level },
        )
        .with_source(source.clone());
        if text.chars().count() > conversion.max_title_chars {
            node.notes.push(OutlineNote::with_label(
                &text,
                "Original heading text",
                Some(source.clone()),
            ));
        }
        Some(builder.append_heading(level, node))
    } else if let Some(list_info) = list {
        let ordered = numbering.is_ordered(list_info);
        let level = list_info
            .level
            .saturating_add(1)
            .clamp(1, conversion.max_depth);
        let (title, truncated) = truncate_text(&text, conversion.max_title_chars);
        if truncated {
            document.stats.truncated_titles += 1;
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::TitleTruncated,
                "DOCX list item was truncated",
                Some(source.clone()),
            ));
        }
        let mut node = OutlineNode::structure(
            if title.is_empty() {
                "Untitled list item"
            } else {
                &title
            },
            StructureKind::ListItem { ordered, level },
        )
        .with_source(source.clone());
        if conversion.preserve_original_paragraphs && !text.is_empty() {
            node.notes.push(OutlineNote::with_label(
                &text,
                "Original list item",
                Some(source.clone()),
            ));
        }
        let path = builder.append_list_item(level, node, conversion.max_depth);
        if conversion.split_content && !text.is_empty() {
            promote_first_sentence(builder, &path, &text, &source, conversion, document)?;
        }
        Some(path)
    } else {
        builder.close_list();
        if !text.is_empty() {
            if builder.current_path().is_empty() {
                builder.ensure_body_section("Body");
            }
            let parent = builder
                .current_node_mut()
                .expect("body section must exist for paragraph content");
            if quote_style {
                let mut options = conversion.clone();
                options.split_content = false;
                let report =
                    add_paragraph_as_content_nodes(parent, &text, Some(source.clone()), &options)?;
                record_expansion(document, report);
                let Some(node) = parent.children.last_mut() else {
                    return Ok(());
                };
                node.kind = crate::mindmap_import::model::OutlineNodeKind::Content {
                    kind: ContentKind::Quote,
                    split_from_paragraph: false,
                };
            } else {
                let report = add_paragraph_as_content_nodes(
                    parent,
                    &text,
                    Some(source.clone()),
                    conversion,
                )?;
                record_expansion(document, report);
            }
        }
        Some(builder.current_path().to_vec())
    };

    let attachment_path = target_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| builder.current_path().to_vec());
    if !attachment_path.is_empty() {
        if let Some(target) = builder.node_mut(&attachment_path) {
            append_referenced_notes(&assets, target, &source, reference_notes);
            target.links.append(&mut assets.links);
            target.attachments.append(&mut assets.attachments);
        }
    } else {
        document.links.append(&mut assets.links);
        document.attachments.append(&mut assets.attachments);
    }
    document.warnings.append(&mut assets.warnings);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_table(
    table: Node<'_, '_>,
    archive: &DocxArchive,
    relationships: &Relationships,
    document: &mut OutlineDocument,
    builder: &mut OutlineTreeBuilder,
    state: &mut DocumentState,
    paragraph_index: &mut usize,
    reference_notes: &mut ReferenceNotes<'_>,
) -> Result<(), ImportError> {
    state.table_count += 1;
    let source = paragraph_source(table, *paragraph_index);
    let mut rows = Vec::<Vec<String>>::new();
    let mut first_row_is_header = false;
    let mut links = Vec::new();
    let mut attachments = Vec::new();
    let mut warnings = Vec::new();
    let mut reference_assets = ParagraphAssets::default();
    let mut table_has_merges = false;

    for (row_index, row) in table
        .children()
        .filter(|node| tag_is(*node, W_NS, "tr"))
        .enumerate()
    {
        if row_index == 0 {
            first_row_is_header = row
                .descendants()
                .find(|node| tag_is(*node, W_NS, "tblHeader"))
                .and_then(|node| attribute_local(node, "val"))
                .is_none_or(|value| !matches!(value, "0" | "false" | "off"));
        }
        let mut cells = Vec::new();
        for cell in row.children().filter(|node| tag_is(*node, W_NS, "tc")) {
            if cell
                .descendants()
                .any(|node| tag_is(node, W_NS, "gridSpan") || tag_is(node, W_NS, "vMerge"))
            {
                table_has_merges = true;
            }
            let mut paragraphs = Vec::new();
            for paragraph in cell.descendants().filter(|node| tag_is(*node, W_NS, "p")) {
                *paragraph_index += 1;
                let text = paragraph_text(paragraph);
                if !text.is_empty() {
                    paragraphs.push(text);
                }
                let mut assets = collect_paragraph_assets(
                    paragraph,
                    archive,
                    relationships,
                    &paragraph_source(paragraph, *paragraph_index),
                )?;
                links.append(&mut assets.links);
                attachments.append(&mut assets.attachments);
                warnings.append(&mut assets.warnings);
                reference_assets
                    .footnote_ids
                    .append(&mut assets.footnote_ids);
                reference_assets.endnote_ids.append(&mut assets.endnote_ids);
                reference_assets.comment_ids.append(&mut assets.comment_ids);
            }
            cells.push(paragraphs.join("\n"));
        }
        rows.push(cells);
    }

    if table_has_merges {
        warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            "DOCX table contains merged cells; cell text was retained without merge geometry",
            Some(source.clone()),
        ));
    }
    while rows
        .last()
        .is_some_and(|row| row.iter().all(String::is_empty))
    {
        rows.pop();
    }
    if rows.is_empty() {
        return Ok(());
    }
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    for row in &mut rows {
        row.resize(column_count, String::new());
    }
    let headers = if first_row_is_header && rows.len() > 1 {
        rows.remove(0)
    } else {
        Vec::new()
    };
    let outline_table = OutlineTable {
        caption: None,
        headers,
        rows,
        source: Some(source.clone()),
    };

    if builder.current_path().is_empty() {
        builder.ensure_body_section("Body");
    }
    if let Some(target) = builder.current_node_mut() {
        target.tables.push(outline_table);
        append_referenced_notes(&reference_assets, target, &source, reference_notes);
        target.links.append(&mut links);
        target.attachments.append(&mut attachments);
    } else {
        document.tables.push(outline_table);
        document.links.append(&mut links);
        document.attachments.append(&mut attachments);
    }
    document.warnings.append(&mut warnings);
    Ok(())
}

fn collect_paragraph_assets(
    paragraph: Node<'_, '_>,
    archive: &DocxArchive,
    relationships: &Relationships,
    source: &SourceLocation,
) -> Result<ParagraphAssets, ImportError> {
    let mut assets = ParagraphAssets::default();
    for hyperlink in paragraph
        .descendants()
        .filter(|node| tag_is(*node, W_NS, "hyperlink"))
    {
        let relationship_id = attribute_ns(hyperlink, R_NS, "id");
        let label = text_of_node(hyperlink);
        if let Some(id) = relationship_id {
            if let Some(relation) = relationships.get(id) {
                assets.links.push(OutlineLink {
                    label: (!label.is_empty()).then_some(label),
                    target: relation.target.clone(),
                    kind: LinkKind::Hyperlink,
                    source: Some(source.clone()),
                });
            } else {
                assets.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::UnsupportedElement,
                    format!("DOCX hyperlink relationship {id} was missing"),
                    Some(source.clone()),
                ));
            }
        } else if let Some(anchor) = attribute_local(hyperlink, "anchor") {
            assets.links.push(OutlineLink {
                label: (!label.is_empty()).then_some(label),
                target: format!("#{anchor}"),
                kind: LinkKind::Hyperlink,
                source: Some(source.clone()),
            });
        }
    }

    let mut image_ids = BTreeSet::new();
    for drawing in paragraph
        .descendants()
        .filter(|node| tag_is(*node, A_NS, "blip") || tag_is(*node, V_NS, "imagedata"))
    {
        if let Some(id) = attribute_ns(drawing, R_NS, "embed")
            .or_else(|| attribute_ns(drawing, R_NS, "id"))
            .or_else(|| attribute_local(drawing, "embed"))
        {
            image_ids.insert(id.to_string());
        }
    }
    for id in image_ids {
        if let Some(relation) = relationships.get(&id) {
            if relation.external {
                assets.links.push(OutlineLink {
                    label: Some("Image".to_string()),
                    target: relation.target.clone(),
                    kind: LinkKind::Image,
                    source: Some(source.clone()),
                });
                continue;
            }
            let path = resolve_internal_target("word", &relation.target);
            let data = archive.media(&path)?;
            let data_missing = data.is_none();
            let name = Path::new(&path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("embedded-image")
                .to_string();
            assets.attachments.push(OutlineAttachment {
                name,
                media_type: mime_type_for_path(&path).map(ToString::to_string),
                uri: Some(path.clone()),
                data,
                source: Some(source.clone()),
            });
            if data_missing {
                assets.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::AttachmentReferenceOnly,
                    format!("DOCX image target {path} was not present in the package"),
                    Some(source.clone()),
                ));
            }
        } else {
            assets.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::UnsupportedElement,
                format!("DOCX image relationship {id} was missing"),
                Some(source.clone()),
            ));
        }
    }

    for object in paragraph
        .descendants()
        .filter(|node| tag_is(*node, W_NS, "object"))
    {
        if let Some(node) = object.descendants().find(|node| {
            tag_is(
                *node,
                "urn:schemas-microsoft-com:office:office",
                "OLEObject",
            )
        }) && let Some(id) = attribute_ns(node, R_NS, "id")
        {
            if let Some(relation) = relationships.get(id) {
                let path = resolve_internal_target("word", &relation.target);
                assets.attachments.push(OutlineAttachment {
                    name: Path::new(&path)
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("embedded-object")
                        .to_string(),
                    media_type: None,
                    uri: Some(path.clone()),
                    data: archive.media(&path)?,
                    source: Some(source.clone()),
                });
            } else {
                assets.warnings.push(ConversionWarning::warning(
                    ConversionWarningCode::UnsupportedElement,
                    "DOCX embedded object relationship was missing",
                    Some(source.clone()),
                ));
            }
        }
    }

    for reference in paragraph.descendants() {
        if tag_is(reference, W_NS, "footnoteReference")
            && let Some(id) = attribute_local(reference, "id")
        {
            assets.footnote_ids.push(id.to_string());
        } else if tag_is(reference, W_NS, "endnoteReference")
            && let Some(id) = attribute_local(reference, "id")
        {
            assets.endnote_ids.push(id.to_string());
        } else if tag_is(reference, W_NS, "commentReference")
            && let Some(id) = attribute_local(reference, "id")
        {
            assets.comment_ids.push(id.to_string());
        }
    }
    Ok(assets)
}

struct ReferenceNotes<'a> {
    footnotes: &'a BTreeMap<String, NoteRecord>,
    endnotes: &'a BTreeMap<String, NoteRecord>,
    comments: &'a BTreeMap<String, NoteRecord>,
    referenced_footnotes: &'a mut BTreeSet<String>,
    referenced_endnotes: &'a mut BTreeSet<String>,
    referenced_comments: &'a mut BTreeSet<String>,
}

fn append_referenced_notes(
    assets: &ParagraphAssets,
    target: &mut OutlineNode,
    source: &SourceLocation,
    notes: &mut ReferenceNotes<'_>,
) {
    for id in &assets.footnote_ids {
        notes.referenced_footnotes.insert(id.clone());
        if let Some(note) = notes.footnotes.get(id) {
            target.notes.push(OutlineNote::with_label(
                &note.text,
                format!("Footnote {id}"),
                Some(source.clone()),
            ));
        }
    }
    for id in &assets.endnote_ids {
        notes.referenced_endnotes.insert(id.clone());
        if let Some(note) = notes.endnotes.get(id) {
            target.notes.push(OutlineNote::with_label(
                &note.text,
                format!("Endnote {id}"),
                Some(source.clone()),
            ));
        }
    }
    for id in &assets.comment_ids {
        notes.referenced_comments.insert(id.clone());
        if let Some(comment) = notes.comments.get(id) {
            let label = match (&comment.author, &comment.date) {
                (Some(author), Some(date)) => format!("Comment {id} — {author} — {date}"),
                (Some(author), None) => format!("Comment {id} — {author}"),
                (None, Some(date)) => format!("Comment {id} — {date}"),
                (None, None) => format!("Comment {id}"),
            };
            target.notes.push(OutlineNote::with_label(
                &comment.text,
                label,
                Some(source.clone()),
            ));
        }
    }
}

fn parse_notes(
    archive: &DocxArchive,
    entry: &str,
    tag: &str,
) -> Result<BTreeMap<String, NoteRecord>, ImportError> {
    let Some(document) = archive.xml(entry)? else {
        return Ok(BTreeMap::new());
    };
    let mut notes = BTreeMap::new();
    for note in document
        .descendants()
        .filter(|node| tag_is(*node, W_NS, tag))
    {
        let note_type = attribute_local(note, "type").unwrap_or_default();
        if matches!(
            note_type,
            "separator" | "continuationSeparator" | "continuationNotice"
        ) {
            continue;
        }
        let Some(id) = attribute_local(note, "id") else {
            continue;
        };
        let text = note
            .children()
            .filter(|node| tag_is(*node, W_NS, "p"))
            .map(paragraph_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            notes.insert(
                id.to_string(),
                NoteRecord {
                    author: None,
                    date: None,
                    text,
                },
            );
        }
    }
    Ok(notes)
}

fn parse_comments(archive: &DocxArchive) -> Result<BTreeMap<String, NoteRecord>, ImportError> {
    let Some(document) = archive.xml("word/comments.xml")? else {
        return Ok(BTreeMap::new());
    };
    let mut comments = BTreeMap::new();
    for comment in document
        .descendants()
        .filter(|node| tag_is(*node, W_NS, "comment"))
    {
        let Some(id) = attribute_local(comment, "id") else {
            continue;
        };
        let text = comment
            .children()
            .filter(|node| tag_is(*node, W_NS, "p"))
            .map(paragraph_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            comments.insert(
                id.to_string(),
                NoteRecord {
                    author: attribute_local(comment, "author").map(ToString::to_string),
                    date: attribute_local(comment, "date").map(ToString::to_string),
                    text,
                },
            );
        }
    }
    Ok(comments)
}

fn parse_core_title(archive: &DocxArchive) -> Result<Option<String>, ImportError> {
    let Some(document) = archive.xml("docProps/core.xml")? else {
        return Ok(None);
    };
    Ok(document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "title")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string))
}

fn append_unreferenced_notes(
    document: &mut OutlineDocument,
    footnotes: &BTreeMap<String, NoteRecord>,
    endnotes: &BTreeMap<String, NoteRecord>,
    comments: &BTreeMap<String, NoteRecord>,
    referenced_footnotes: &BTreeSet<String>,
    referenced_endnotes: &BTreeSet<String>,
    referenced_comments: &BTreeSet<String>,
) {
    for (id, note) in footnotes {
        if !referenced_footnotes.contains(id) {
            document.notes.push(OutlineNote::with_label(
                &note.text,
                format!("Unreferenced footnote {id}"),
                None,
            ));
        }
    }
    for (id, note) in endnotes {
        if !referenced_endnotes.contains(id) {
            document.notes.push(OutlineNote::with_label(
                &note.text,
                format!("Unreferenced endnote {id}"),
                None,
            ));
        }
    }
    for (id, comment) in comments {
        if !referenced_comments.contains(id) {
            document.notes.push(OutlineNote::with_label(
                &comment.text,
                format!("Unreferenced comment {id}"),
                None,
            ));
        }
    }
}

fn add_docx_warnings(
    document: &mut OutlineDocument,
    state: &DocumentState,
    footnotes: &BTreeMap<String, NoteRecord>,
    endnotes: &BTreeMap<String, NoteRecord>,
    comments: &BTreeMap<String, NoteRecord>,
) {
    if !comments.is_empty() {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            format!(
                "DOCX contains {} comment(s); comment text was preserved as notes",
                comments.len()
            ),
            None,
        ));
    }
    if state.tracked_change_count > 0 {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            format!(
                "DOCX contains {} tracked-change marker(s); inserted text was kept and deleted text was ignored",
                state.tracked_change_count
            ),
            None,
        ));
    }
    for style in &state.unknown_styles {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            format!("DOCX paragraph style \"{style}\" was treated as body text"),
            None,
        ));
    }
    if !footnotes.is_empty() {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            format!(
                "{} DOCX footnote(s) were preserved as topic notes",
                footnotes.len()
            ),
            None,
        ));
    }
    if !endnotes.is_empty() {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::UnsupportedElement,
            format!(
                "{} DOCX endnote(s) were preserved as topic notes",
                endnotes.len()
            ),
            None,
        ));
    }
    if state.used_heuristic_heading {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::InvalidStructure,
            "DOCX headings were inferred from bold/large text because structural heading styles were absent",
            None,
        ));
    }
}

fn document_has_structural_heading(body: Node<'_, '_>, styles: &StyleSheet) -> bool {
    body.descendants()
        .filter(|node| tag_is(*node, W_NS, "p"))
        .any(|paragraph| {
            if paragraph_outline_level(paragraph).is_some() {
                return true;
            }
            let style_id = paragraph_style_id(paragraph);
            let resolved = styles.resolve(style_id);
            resolved.outline_level.is_some()
                || resolved.heading_level.is_some()
                || resolved.title_like
        })
}

fn count_tracked_changes(body: Node<'_, '_>, state: &mut DocumentState) {
    state.tracked_change_count = body
        .descendants()
        .filter(|node| {
            tag_is(*node, W_NS, "ins")
                || tag_is(*node, W_NS, "del")
                || tag_is(*node, W_NS, "moveFrom")
                || tag_is(*node, W_NS, "moveTo")
        })
        .count();
}

fn paragraph_style_id<'a, 'input>(paragraph: Node<'a, 'input>) -> Option<&'a str> {
    paragraph
        .children()
        .find(|node| tag_is(*node, W_NS, "pPr"))
        .and_then(|properties| {
            properties
                .children()
                .find(|node| tag_is(*node, W_NS, "pStyle"))
        })
        .and_then(|style| attribute_local(style, "val"))
}

fn paragraph_outline_level(paragraph: Node<'_, '_>) -> Option<u8> {
    paragraph
        .descendants()
        .filter(|node| tag_is(*node, W_NS, "outlineLvl"))
        .find_map(|node| {
            attribute_local(node, "val")
                .and_then(|value| value.parse::<u8>().ok())
                .map(|level| level.saturating_add(1).min(6))
        })
}

fn paragraph_list_info(paragraph: Node<'_, '_>) -> Option<ListInfo> {
    let properties = paragraph
        .children()
        .find(|node| tag_is(*node, W_NS, "pPr"))?;
    let numbering = properties
        .children()
        .find(|node| tag_is(*node, W_NS, "numPr"))?;
    let num_id = numbering
        .children()
        .find(|node| tag_is(*node, W_NS, "numId"))
        .and_then(|node| attribute_local(node, "val"))
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if num_id == 0 {
        return None;
    }
    let level = numbering
        .children()
        .find(|node| tag_is(*node, W_NS, "ilvl"))
        .and_then(|node| attribute_local(node, "val"))
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0);
    Some(ListInfo { num_id, level })
}

fn heuristic_heading_level(paragraph: Node<'_, '_>, text: &str) -> Option<u8> {
    if text.trim().is_empty()
        || text.chars().count() > 80
        || text.contains("://")
        || text.chars().last().is_some_and(|character| {
            matches!(character, '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';')
        })
    {
        return None;
    }
    let font_size = paragraph
        .descendants()
        .filter(|node| tag_is(*node, W_NS, "sz"))
        .filter_map(|node| attribute_local(node, "val").and_then(|value| value.parse::<u32>().ok()))
        .max()
        .unwrap_or(0);
    let bold = paragraph.descendants().any(|node| {
        (tag_is(node, W_NS, "b") || tag_is(node, W_NS, "bCs"))
            && attribute_local(node, "val")
                .is_none_or(|value| !matches!(value, "0" | "false" | "off"))
    });
    if font_size >= 32 {
        Some(1)
    } else if font_size >= 28 {
        Some(2)
    } else if bold && font_size >= 24 {
        Some(3)
    } else if bold && font_size >= 22 {
        Some(4)
    } else {
        None
    }
}

fn heading_level_from_style(style_id: &str, name: &str) -> Option<u8> {
    let normalized_name = normalize_style_name(name);
    let normalized_id = normalize_style_name(style_id);
    heading_regex()
        .captures(&normalized_name)
        .or_else(|| heading_regex().captures(&normalized_id))
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<u8>().ok())
        .map(|level| level.clamp(1, 6))
}

fn is_title_style(style_id: &str, name: &str) -> bool {
    match normalize_style_name(name).as_str() {
        "title" | "标题" => true,
        _ => matches!(normalize_style_name(style_id).as_str(), "title" | "标题"),
    }
}

fn normalize_style_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn heading_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?:heading|标题|見出し)([1-9])$").expect("valid heading style regex")
    })
}

fn paragraph_text(paragraph: Node<'_, '_>) -> String {
    let mut output = String::new();
    append_text(paragraph, &mut output);
    normalize_whitespace(&output)
}

fn text_of_node(node: Node<'_, '_>) -> String {
    let mut output = String::new();
    append_text(node, &mut output);
    normalize_whitespace(&output)
}

fn append_text(node: Node<'_, '_>, output: &mut String) {
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text()
                && !inside_ignored_text_node(child)
            {
                output.push_str(text);
            }
            continue;
        }
        if !child.is_element() {
            continue;
        }
        if tag_is(child, W_NS, "del") || tag_is(child, W_NS, "moveFrom") {
            continue;
        }
        if tag_is(child, W_NS, "t") {
            if let Some(text) = child.text() {
                output.push_str(text);
            }
        } else if tag_is(child, W_NS, "tab") {
            output.push('\t');
        } else if tag_is(child, W_NS, "br") || tag_is(child, W_NS, "cr") {
            output.push('\n');
        } else if tag_is(child, W_NS, "noBreakHyphen") {
            output.push('-');
        } else {
            append_text(child, output);
        }
    }
}

fn inside_ignored_text_node(node: Node<'_, '_>) -> bool {
    node.ancestors()
        .any(|ancestor| tag_is(ancestor, W_NS, "del") || tag_is(ancestor, W_NS, "moveFrom"))
}

fn normalize_whitespace(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn promote_first_sentence(
    builder: &mut OutlineTreeBuilder,
    path: &[usize],
    text: &str,
    source: &SourceLocation,
    conversion: &ConversionOptions,
    document: &mut OutlineDocument,
) -> Result<(), ImportError> {
    use crate::mindmap_import::segment::{SentenceSplitOptions, segment_sentences};

    let fragments = segment_sentences(
        text,
        &SentenceSplitOptions {
            min_fragment_chars: conversion.min_fragment_chars,
            merge_short_fragments: conversion.merge_short_fragments,
        },
    );
    if fragments.len() < 2 {
        return Ok(());
    }
    let Some(node) = builder.node_mut(path) else {
        return Ok(());
    };
    let (title, truncated) = truncate_text(&fragments[0].text, conversion.max_title_chars);
    node.title = if title.is_empty() {
        "Untitled list item".to_string()
    } else {
        title
    };
    if truncated {
        document.stats.truncated_titles += 1;
    }
    for fragment in fragments.iter().skip(1) {
        let (fragment_title, fragment_truncated) =
            truncate_text(&fragment.text, conversion.max_title_chars);
        if fragment_truncated {
            document.stats.truncated_titles += 1;
        }
        node.children.push(
            OutlineNode::content(
                if fragment_title.is_empty() {
                    "Untitled fragment".to_string()
                } else {
                    fragment_title
                },
                ContentKind::Sentence,
            )
            .with_source(source.clone()),
        );
        document.stats.split_fragments += 1;
    }
    Ok(())
}

fn record_expansion(document: &mut OutlineDocument, report: ParagraphExpansionReport) {
    document.stats.split_fragments += report.nodes_added;
    document.stats.merged_fragments += report.merged_fragments;
    document.stats.skipped_items += report
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning.code,
                ConversionWarningCode::ContentSkipped | ConversionWarningCode::EmptyInput
            )
        })
        .count();
    document.warnings.extend(report.warnings);
}

fn paragraph_source(node: Node<'_, '_>, index: usize) -> SourceLocation {
    SourceLocation {
        path: Vec::new(),
        line: u32::try_from(index).ok(),
        column: None,
        byte_offset: Some(node.range().start),
    }
}

fn resolve_internal_target(base: &str, target: &str) -> String {
    let target = target.trim_start_matches('/');
    let candidate = if target.starts_with("word/") {
        PathBuf::from(target)
    } else {
        Path::new(base).join(target)
    };
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    normalized.to_string_lossy().replace('\\', "/")
}

fn mime_type_for_path(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("bmp") => Some("image/bmp"),
        Some("webp") => Some("image/webp"),
        Some("svg") => Some("image/svg+xml"),
        Some("emf") => Some("image/emf"),
        Some("wmf") => Some("image/wmf"),
        Some("bin") => Some("application/octet-stream"),
        _ => None,
    }
}

fn tag_is(node: Node<'_, '_>, namespace: &str, name: &str) -> bool {
    node.is_element()
        && node.tag_name().name() == name
        && node.tag_name().namespace() == Some(namespace)
}

fn attribute_local<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

fn attribute_ns<'a, 'input>(
    node: Node<'a, 'input>,
    namespace: &str,
    name: &str,
) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name && attribute.namespace() == Some(namespace))
        .map(|attribute| attribute.value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_title_styles_recognized_without_font_guessing() {
        assert_eq!(heading_level_from_style("Heading1", "Heading 1"), Some(1));
        assert_eq!(heading_level_from_style("Custom", "标题 3"), Some(3));
        assert!(is_title_style("Title", "Title"));
        assert_eq!(heading_level_from_style("Normal", "Normal"), None);
    }

    #[test]
    fn internal_relationship_targets_are_normalized() {
        assert_eq!(
            resolve_internal_target("word", "media/image1.png"),
            "word/media/image1.png"
        );
        assert_eq!(
            resolve_internal_target("word", "/word/media/image1.png"),
            "word/media/image1.png"
        );
    }
}
