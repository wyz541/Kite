//! Small shared helpers for source-format adapters.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::mindmap_import::model::{
    ConversionOptions, ConversionStats, ImportError, OutlineDocument, OutlineNode, SourceFormat,
    SourceLocation, SourceProvenance,
};
use crate::mindmap_import::segment::ParagraphExpansionReport;

pub(crate) const ADAPTER_VERSION: &str = "1";

pub(crate) fn decode_utf8(bytes: &[u8]) -> Result<String, ImportError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ImportError::InvalidDocument(format!("source is not valid UTF-8: {error}"))
    })?;
    Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_string())
}

pub(crate) fn provenance(
    format: SourceFormat,
    adapter: &str,
    source: Option<&Path>,
    text: &str,
) -> SourceProvenance {
    let mut provenance = SourceProvenance::new(format, adapter, ADAPTER_VERSION);
    provenance.source_path = source.map(|path| path.to_string_lossy().into_owned());
    provenance.source_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    provenance
}

pub(crate) fn fallback_title(source: Option<&Path>, default: &str) -> String {
    source
        .and_then(Path::file_stem)
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub(crate) fn source_location(
    path: Vec<String>,
    line: Option<usize>,
    column: Option<usize>,
    byte_offset: Option<usize>,
) -> SourceLocation {
    SourceLocation {
        path,
        line: line.map(|value| value as u32),
        column: column.map(|value| value as u32),
        byte_offset,
    }
}

pub(crate) fn truncate_title(
    title: &str,
    source: Option<SourceLocation>,
    options: &ConversionOptions,
    stats: &mut ConversionStats,
    warnings: &mut Vec<crate::mindmap_import::ConversionWarning>,
) -> String {
    let (title, truncated) = crate::mindmap_import::truncate_text(title, options.max_title_chars);
    if truncated {
        stats.truncated_titles += 1;
        warnings.push(crate::mindmap_import::ConversionWarning::warning(
            crate::mindmap_import::ConversionWarningCode::TitleTruncated,
            format!(
                "Topic title was truncated to {} characters",
                options.max_title_chars
            ),
            source,
        ));
    }
    if title.is_empty() {
        "Untitled topic".to_string()
    } else {
        title
    }
}

pub(crate) fn accumulate_report(document: &mut OutlineDocument, report: ParagraphExpansionReport) {
    document.stats.split_fragments += report.nodes_added;
    document.stats.merged_fragments += report.merged_fragments;
    document.warnings.extend(report.warnings);
}

pub(crate) fn node_at_mut<'a>(
    document: &'a mut OutlineDocument,
    path: &[usize],
) -> Option<&'a mut OutlineNode> {
    let (first, rest) = path.split_first()?;
    let mut node = document.children.get_mut(*first)?;
    for index in rest {
        node = node.children.get_mut(*index)?;
    }
    Some(node)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HeadingStack {
    frames: Vec<(u8, Vec<usize>)>,
}

impl HeadingStack {
    pub(crate) fn parent_for(&mut self, level: u8) -> Option<Vec<usize>> {
        while self
            .frames
            .last()
            .is_some_and(|(candidate, _)| *candidate >= level)
        {
            self.frames.pop();
        }
        self.frames.last().map(|(_, path)| path.clone())
    }

    pub(crate) fn record(&mut self, level: u8, path: Vec<usize>) {
        self.frames.push((level, path));
    }

    pub(crate) fn current(&self) -> Option<Vec<usize>> {
        self.frames.last().map(|(_, path)| path.clone())
    }
}
