//! Shared import model and adapter boundary.
//!
//! Format adapters are responsible only for extracting document semantics into
//! [`OutlineDocument`]. XMind serialization is deliberately kept in the writer so
//! every adapter follows the same mapping and reporting rules.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::mindmap::XmindError;

pub const OUTLINE_SCHEMA_VERSION: u32 = 1;
pub const IMPORT_PIPELINE_VERSION: &str = "1";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    #[default]
    Markdown,
    Docx,
    Text,
    Html,
    Opml,
    Pdf,
    Other(String),
}

impl std::fmt::Display for SourceFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Markdown => formatter.write_str("markdown"),
            Self::Docx => formatter.write_str("docx"),
            Self::Text => formatter.write_str("text"),
            Self::Html => formatter.write_str("html"),
            Self::Opml => formatter.write_str("opml"),
            Self::Pdf => formatter.write_str("pdf"),
            Self::Other(value) => formatter.write_str(value),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionPreset {
    #[default]
    StructureFirst,
    ExpandedContent,
    StrictOutline,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionOptions {
    pub preset: ConversionPreset,
    pub split_content: bool,
    pub preserve_original_paragraphs: bool,
    pub merge_short_fragments: bool,
    /// Fragments with fewer non-whitespace, non-punctuation characters are
    /// merged into an adjacent sentence.
    pub min_fragment_chars: usize,
    pub max_title_chars: usize,
    pub max_nodes: usize,
    pub max_depth: u8,
    pub include_source_path: bool,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            preset: ConversionPreset::StructureFirst,
            split_content: true,
            preserve_original_paragraphs: true,
            merge_short_fragments: true,
            min_fragment_chars: 12,
            max_title_chars: 120,
            max_nodes: 10_000,
            max_depth: 8,
            include_source_path: false,
        }
    }
}

impl ConversionOptions {
    pub fn strict_outline() -> Self {
        Self {
            preset: ConversionPreset::StrictOutline,
            split_content: false,
            preserve_original_paragraphs: true,
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<(), ImportError> {
        if self.min_fragment_chars == 0 {
            return Err(ImportError::InvalidOptions(
                "min_fragment_chars must be greater than zero".to_string(),
            ));
        }
        if self.max_title_chars == 0 {
            return Err(ImportError::InvalidOptions(
                "max_title_chars must be greater than zero".to_string(),
            ));
        }
        if self.max_nodes == 0 {
            return Err(ImportError::InvalidOptions(
                "max_nodes must be greater than zero".to_string(),
            ));
        }
        if self.max_depth == 0 {
            return Err(ImportError::InvalidOptions(
                "max_depth must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructureKind {
    Document,
    Heading { level: u8 },
    Section,
    ListItem { ordered: bool, level: u8 },
    OutlineItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Paragraph,
    Sentence,
    Quote,
    CodeBlock,
    Task,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum OutlineNodeKind {
    Structure {
        kind: StructureKind,
    },
    Content {
        kind: ContentKind,
        split_from_paragraph: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Stable logical path, such as `["Performance", "Indexing"]`. It is not a
    /// filesystem path.
    pub path: Vec<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub byte_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteFormat {
    Plain,
    Markdown,
    Html,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNote {
    pub text: String,
    pub format: NoteFormat,
    pub label: Option<String>,
    pub source: Option<SourceLocation>,
}

impl OutlineNote {
    pub fn plain(text: impl Into<String>, source: Option<SourceLocation>) -> Self {
        Self {
            text: text.into(),
            format: NoteFormat::Plain,
            label: None,
            source,
        }
    }

    pub fn with_label(
        text: impl Into<String>,
        label: impl Into<String>,
        source: Option<SourceLocation>,
    ) -> Self {
        Self {
            text: text.into(),
            format: NoteFormat::Plain,
            label: Some(label.into()),
            source,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    #[default]
    Hyperlink,
    Image,
    Attachment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineLink {
    pub label: Option<String>,
    pub target: String,
    pub kind: LinkKind,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineAttachment {
    pub name: String,
    pub media_type: Option<String>,
    pub uri: Option<String>,
    /// Binary payloads are represented in the shared model but packaging them
    /// as XMind attachment resources is a later compatibility slice.
    pub data: Option<Vec<u8>>,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineTable {
    pub caption: Option<String>,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub source: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineNode {
    pub title: String,
    pub kind: OutlineNodeKind,
    pub children: Vec<OutlineNode>,
    pub notes: Vec<OutlineNote>,
    pub links: Vec<OutlineLink>,
    pub attachments: Vec<OutlineAttachment>,
    pub tables: Vec<OutlineTable>,
    pub source: Option<SourceLocation>,
}

impl OutlineNode {
    pub fn structure(title: impl Into<String>, kind: StructureKind) -> Self {
        Self {
            title: title.into(),
            kind: OutlineNodeKind::Structure { kind },
            children: Vec::new(),
            notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            tables: Vec::new(),
            source: None,
        }
    }

    pub fn content(title: impl Into<String>, kind: ContentKind) -> Self {
        Self {
            title: title.into(),
            kind: OutlineNodeKind::Content {
                kind,
                split_from_paragraph: false,
            },
            children: Vec::new(),
            notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            tables: Vec::new(),
            source: None,
        }
    }

    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = Some(source);
        self
    }

    pub fn is_structure(&self) -> bool {
        matches!(self.kind, OutlineNodeKind::Structure { .. })
    }

    pub fn is_content(&self) -> bool {
        matches!(self.kind, OutlineNodeKind::Content { .. })
    }

    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
    }

    pub fn structure_node_count(&self) -> usize {
        usize::from(self.is_structure())
            + self
                .children
                .iter()
                .map(Self::structure_node_count)
                .sum::<usize>()
    }

    pub fn content_node_count(&self) -> usize {
        usize::from(self.is_content())
            + self
                .children
                .iter()
                .map(Self::content_node_count)
                .sum::<usize>()
    }

    pub fn note_count(&self) -> usize {
        self.notes.len() + self.children.iter().map(Self::note_count).sum::<usize>()
    }

    pub fn link_count(&self) -> usize {
        self.links.len() + self.children.iter().map(Self::link_count).sum::<usize>()
    }

    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
            + self
                .children
                .iter()
                .map(Self::attachment_count)
                .sum::<usize>()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len() + self.children.iter().map(Self::table_count).sum::<usize>()
    }

    pub fn max_depth_from_here(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(Self::max_depth_from_here)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub format: SourceFormat,
    pub source_path: Option<String>,
    pub source_sha256: String,
    pub adapter: String,
    pub adapter_version: String,
    /// Adapter-specific metadata. `BTreeMap` keeps deterministic ordering.
    pub original_format: std::collections::BTreeMap<String, Value>,
}

impl SourceProvenance {
    pub fn new(
        format: SourceFormat,
        adapter: impl Into<String>,
        adapter_version: impl Into<String>,
    ) -> Self {
        Self {
            format,
            source_path: None,
            source_sha256: String::new(),
            adapter: adapter.into(),
            adapter_version: adapter_version.into(),
            original_format: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionStats {
    pub split_fragments: usize,
    pub merged_fragments: usize,
    pub truncated_titles: usize,
    pub skipped_items: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineDocument {
    pub schema_version: u32,
    pub title: String,
    pub children: Vec<OutlineNode>,
    pub notes: Vec<OutlineNote>,
    pub links: Vec<OutlineLink>,
    pub attachments: Vec<OutlineAttachment>,
    pub tables: Vec<OutlineTable>,
    pub warnings: Vec<ConversionWarning>,
    pub stats: ConversionStats,
    pub source: SourceProvenance,
}

impl OutlineDocument {
    pub fn new(title: impl Into<String>, source: SourceProvenance) -> Self {
        Self {
            schema_version: OUTLINE_SCHEMA_VERSION,
            title: title.into(),
            children: Vec::new(),
            notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            tables: Vec::new(),
            warnings: Vec::new(),
            stats: ConversionStats::default(),
            source,
        }
    }

    pub fn node_count(&self) -> usize {
        self.children.iter().map(OutlineNode::node_count).sum()
    }

    pub fn structure_node_count(&self) -> usize {
        self.children
            .iter()
            .map(OutlineNode::structure_node_count)
            .sum()
    }

    pub fn content_node_count(&self) -> usize {
        self.children
            .iter()
            .map(OutlineNode::content_node_count)
            .sum()
    }

    pub fn note_count(&self) -> usize {
        self.notes.len()
            + self
                .children
                .iter()
                .map(OutlineNode::note_count)
                .sum::<usize>()
    }

    pub fn link_count(&self) -> usize {
        self.links.len()
            + self
                .children
                .iter()
                .map(OutlineNode::link_count)
                .sum::<usize>()
    }

    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
            + self
                .children
                .iter()
                .map(OutlineNode::attachment_count)
                .sum::<usize>()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
            + self
                .children
                .iter()
                .map(OutlineNode::table_count)
                .sum::<usize>()
    }

    pub fn max_depth(&self) -> usize {
        self.children
            .iter()
            .map(OutlineNode::max_depth_from_here)
            .max()
            .unwrap_or(0)
    }

    pub fn report(&self, options: &ConversionOptions) -> ConversionReport {
        ConversionReport {
            preset: options.preset.clone(),
            source_format: self.source.format.clone(),
            source_sha256: self.source.source_sha256.clone(),
            node_count: self.node_count(),
            structure_node_count: self.structure_node_count(),
            content_node_count: self.content_node_count(),
            max_depth: self.max_depth(),
            note_count: self.note_count(),
            link_count: self.link_count(),
            attachment_count: self.attachment_count(),
            table_count: self.table_count(),
            stats: self.stats.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionWarningCode {
    EmptyInput,
    UnsupportedElement,
    InvalidStructure,
    TitleTruncated,
    NodeLimitExceeded,
    DepthLimitExceeded,
    TableFlattened,
    AttachmentReferenceOnly,
    ContentSkipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionWarning {
    pub code: ConversionWarningCode,
    pub severity: WarningSeverity,
    pub message: String,
    pub source: Option<SourceLocation>,
}

impl ConversionWarning {
    pub fn warning(
        code: ConversionWarningCode,
        message: impl Into<String>,
        source: Option<SourceLocation>,
    ) -> Self {
        Self {
            code,
            severity: WarningSeverity::Warning,
            message: message.into(),
            source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionReport {
    pub preset: ConversionPreset,
    pub source_format: SourceFormat,
    pub source_sha256: String,
    pub node_count: usize,
    pub structure_node_count: usize,
    pub content_node_count: usize,
    pub max_depth: usize,
    pub note_count: usize,
    pub link_count: usize,
    pub attachment_count: usize,
    pub table_count: usize,
    pub stats: ConversionStats,
    pub warnings: Vec<ConversionWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub name: String,
    pub version: String,
    pub formats: Vec<SourceFormat>,
}

#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    pub path: Option<&'a Path>,
    pub bytes: &'a [u8],
    pub format: &'a SourceFormat,
}

/// A format adapter extracts semantics only. It must never construct XMind JSON
/// or write a package directly.
pub trait OutlineAdapter {
    fn descriptor(&self) -> AdapterDescriptor;

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError>;
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("unsupported source format: {0}")]
    UnsupportedFormat(SourceFormat),
    #[error("invalid source document: {0}")]
    InvalidDocument(String),
    #[error("invalid conversion options: {0}")]
    InvalidOptions(String),
    #[error("{limit} exceeds the configured maximum of {maximum}")]
    LimitExceeded { limit: &'static str, maximum: usize },
    #[error("failed to write XMind package: {0}")]
    Write(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Xmind(#[from] XmindError),
}

pub fn truncate_text(text: &str, max_chars: usize) -> (String, bool) {
    let trimmed = text.trim();
    let mut characters = trimmed.chars();
    let truncated: String = characters.by_ref().take(max_chars).collect();
    let was_truncated = characters.next().is_some();
    let mut result = truncated.trim_end().to_string();
    if was_truncated {
        result.push('…');
    }
    (result, was_truncated)
}

pub(crate) fn stable_id(seed: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = format!("{:x}", Sha256::digest(seed.as_bytes()));
    digest[..32].to_string()
}
