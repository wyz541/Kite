//! Shared document-to-mindmap import foundation.
//!
//! Import adapters extract source semantics into [`OutlineDocument`]. A single
//! XMind writer then applies deterministic mapping, reporting, and package
//! validation for every supported source format.

mod adapter_support;
mod docx;
mod html;
mod markdown;
mod model;
mod opml;
mod segment;
mod text;
mod tree_builder;
mod writer;

pub use docx::{DocxAdapter, DocxAdapterOptions};
pub use html::HtmlAdapter;
pub use markdown::MarkdownAdapter;
pub use model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionPreset, ConversionReport,
    ConversionStats, ConversionWarning, ConversionWarningCode, IMPORT_PIPELINE_VERSION,
    ImportError, LinkKind, NoteFormat, OutlineAdapter, OutlineAttachment, OutlineDocument,
    OutlineLink, OutlineNode, OutlineNodeKind, OutlineNote, OutlineTable, SourceFormat,
    SourceInput, SourceLocation, SourceProvenance, StructureKind, TaskState, WarningSeverity,
    truncate_text,
};
pub use opml::OpmlAdapter;
pub use segment::{
    ParagraphExpansionReport, SentenceSplitOptions, TextFragment, add_paragraph_as_content_nodes,
    segment_sentences,
};
pub use text::{
    TextAdapter, TextAdapterOptions, TextAnalysis, TextConfidence, TextLineAnalysis, TextLineKind,
    TextStrategy,
};
pub use writer::{
    CONTENT_ENTRY, MANIFEST_ENTRY, METADATA_ENTRY, XmindWriteResult, write_xmind_bytes,
    write_xmind_file,
};
