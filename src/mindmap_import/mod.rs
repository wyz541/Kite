//! Shared document-to-mindmap import foundation.
//!
//! Import adapters extract source semantics into [`OutlineDocument`]. A single
//! XMind writer then applies deterministic mapping, reporting, and package
//! validation for every supported source format.

mod model;
mod segment;
mod writer;

pub use model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionPreset, ConversionReport,
    ConversionStats, ConversionWarning, ConversionWarningCode, IMPORT_PIPELINE_VERSION,
    ImportError, LinkKind, NoteFormat, OutlineAdapter, OutlineAttachment, OutlineDocument,
    OutlineLink, OutlineNode, OutlineNodeKind, OutlineNote, OutlineTable, SourceFormat,
    SourceInput, SourceLocation, SourceProvenance, StructureKind, WarningSeverity, truncate_text,
};
pub use segment::{
    ParagraphExpansionReport, SentenceSplitOptions, TextFragment, add_paragraph_as_content_nodes,
    segment_sentences,
};
pub use writer::{
    CONTENT_ENTRY, MANIFEST_ENTRY, METADATA_ENTRY, XmindWriteResult, write_xmind_bytes,
    write_xmind_file,
};
