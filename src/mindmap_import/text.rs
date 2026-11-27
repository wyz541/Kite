//! Plain-text structure detection and conversion into the shared outline model.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::mindmap_import::model::{
    AdapterDescriptor, ContentKind, ConversionOptions, ConversionPreset, ConversionWarning,
    ConversionWarningCode, ImportError, OutlineAdapter, OutlineDocument, OutlineNode,
    OutlineNodeKind, OutlineNote, SourceFormat, SourceInput, SourceLocation, SourceProvenance,
    StructureKind, truncate_text,
};
use crate::mindmap_import::segment::{
    ParagraphExpansionReport, SentenceSplitOptions, add_paragraph_as_content_nodes,
    segment_sentences,
};
use crate::mindmap_import::tree_builder::OutlineTreeBuilder;

const ADAPTER_NAME: &str = "txt-smart-layering";
const ADAPTER_VERSION: &str = "1.0.0";
const LONG_PARAGRAPH_THRESHOLD: usize = 8;
const PARAGRAPH_GROUP_SIZE: usize = 4;
const MAX_TEXT_INPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextStrategy {
    StrictOutline,
    SmartLayering,
    SentenceSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextConfidence {
    Low,
    Medium,
    High,
}

impl TextConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextLineKind {
    Blank,
    Heading { level: u8, inferred: bool },
    ListItem { level: u8, ordered: bool },
    Paragraph,
    ShortLineCandidate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLineAnalysis {
    pub line_number: u32,
    pub text: String,
    pub indent_columns: usize,
    pub confidence: TextConfidence,
    pub kind: TextLineKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextAnalysis {
    pub strategy: TextStrategy,
    pub lines: Vec<TextLineAnalysis>,
    pub heading_count: usize,
    pub inferred_heading_count: usize,
    pub list_item_count: usize,
    pub paragraph_line_count: usize,
    pub ambiguous_short_line_count: usize,
    pub mixed_indentation: bool,
    pub max_depth: u8,
    pub structure_confidence: TextConfidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextAdapterOptions {
    pub short_line_max_chars: usize,
    pub max_inferred_depth: u8,
}

impl Default for TextAdapterOptions {
    fn default() -> Self {
        Self {
            short_line_max_chars: 48,
            max_inferred_depth: 4,
        }
    }
}

impl TextAdapterOptions {
    fn validate(&self) -> Result<(), ImportError> {
        if self.short_line_max_chars == 0 {
            return Err(ImportError::InvalidOptions(
                "short_line_max_chars must be greater than zero".to_string(),
            ));
        }
        if self.max_inferred_depth == 0 {
            return Err(ImportError::InvalidOptions(
                "max_inferred_depth must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TextAdapter {
    options: TextAdapterOptions,
}

impl Default for TextAdapter {
    fn default() -> Self {
        Self::new(TextAdapterOptions::default())
    }
}

impl TextAdapter {
    pub fn new(options: TextAdapterOptions) -> Self {
        Self { options }
    }

    pub fn analyze(
        &self,
        source: SourceInput<'_>,
        conversion: &ConversionOptions,
    ) -> Result<TextAnalysis, ImportError> {
        self.ensure_format(source.format)?;
        ensure_input_size(source.bytes)?;
        conversion.validate()?;
        self.options.validate()?;
        let decoded = decode_text(source.bytes)?;
        Ok(analyze_decoded_text(
            &decoded.text,
            &conversion.preset,
            &self.options,
        ))
    }
}

impl OutlineAdapter for TextAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            name: ADAPTER_NAME.to_string(),
            version: ADAPTER_VERSION.to_string(),
            formats: vec![SourceFormat::Text],
        }
    }

    fn extract(
        &self,
        source: SourceInput<'_>,
        options: &ConversionOptions,
    ) -> Result<OutlineDocument, ImportError> {
        self.ensure_format(source.format)?;
        ensure_input_size(source.bytes)?;
        options.validate()?;
        self.options.validate()?;
        let decoded = decode_text(source.bytes)?;
        let analysis = analyze_decoded_text(&decoded.text, &options.preset, &self.options);
        build_document(source, decoded, analysis, options, &self.options)
    }
}

impl TextAdapter {
    fn ensure_format(&self, format: &SourceFormat) -> Result<(), ImportError> {
        if matches!(format, SourceFormat::Text) {
            Ok(())
        } else {
            Err(ImportError::UnsupportedFormat(format.clone()))
        }
    }
}

#[derive(Clone, Debug)]
struct DecodedText {
    text: String,
    encoding: &'static str,
}

fn ensure_input_size(bytes: &[u8]) -> Result<(), ImportError> {
    if bytes.len() > MAX_TEXT_INPUT_BYTES {
        return Err(ImportError::InvalidDocument(format!(
            "TXT exceeds the {} byte import limit",
            MAX_TEXT_INPUT_BYTES
        )));
    }
    Ok(())
}

fn decode_text(bytes: &[u8]) -> Result<DecodedText, ImportError> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ImportError::InvalidDocument(format!("TXT must be UTF-8 or BOM-marked UTF-16: {error}"))
    })?;
    Ok(DecodedText {
        text: normalize_newlines(text),
        encoding: "utf-8",
    })
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<DecodedText, ImportError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(ImportError::InvalidDocument(
            "UTF-16 TXT has an odd byte length".to_string(),
        ));
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect::<Vec<_>>();
    let text = String::from_utf16(&units)
        .map_err(|error| ImportError::InvalidDocument(format!("invalid UTF-16 TXT: {error}")))?;
    Ok(DecodedText {
        text: normalize_newlines(&text),
        encoding: if little_endian {
            "utf-16le"
        } else {
            "utf-16be"
        },
    })
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[derive(Clone, Debug)]
struct RawLine {
    line_number: u32,
    text: String,
    indent_columns: usize,
    has_tab_indent: bool,
    has_space_indent: bool,
    is_blank: bool,
}

fn raw_lines(text: &str) -> Vec<RawLine> {
    text.split('\n')
        .enumerate()
        .map(|(index, line)| {
            let trimmed_end = line.trim_end();
            let (indent_columns, has_tab_indent, has_space_indent) = measure_indent(trimmed_end);
            let body = trimmed_end.trim_start().to_string();
            RawLine {
                line_number: u32::try_from(index + 1).unwrap_or(u32::MAX),
                text: body,
                indent_columns,
                has_tab_indent,
                has_space_indent,
                is_blank: trimmed_end.trim().is_empty(),
            }
        })
        .collect()
}

fn measure_indent(line: &str) -> (usize, bool, bool) {
    let mut columns = 0;
    let mut tab = false;
    let mut space = false;
    for character in line.chars() {
        match character {
            '\t' => {
                tab = true;
                columns = ((columns / 4) + 1) * 4;
            }
            ' ' => {
                space = true;
                columns += 1;
            }
            '\u{3000}' => {
                space = true;
                columns += 2;
            }
            _ => break,
        }
    }
    (columns, tab, space)
}

fn analyze_decoded_text(
    text: &str,
    preset: &ConversionPreset,
    options: &TextAdapterOptions,
) -> TextAnalysis {
    let strategy = strategy_for_preset(preset);
    let lines = raw_lines(text);
    let indent_levels = indentation_levels(&lines);
    let mixed_indentation = lines
        .iter()
        .any(|line| line.has_tab_indent && line.has_space_indent)
        || (lines.iter().any(|line| line.has_tab_indent)
            && lines.iter().any(|line| line.has_space_indent));
    let mut result = Vec::with_capacity(lines.len());
    let mut heading_count = 0;
    let mut inferred_heading_count = 0;
    let mut list_item_count = 0;
    let mut paragraph_line_count = 0;
    let mut ambiguous_short_line_count = 0;
    let mut max_depth = 0_u8;

    for (index, line) in lines.iter().enumerate() {
        if line.is_blank {
            result.push(TextLineAnalysis {
                line_number: line.line_number,
                text: String::new(),
                indent_columns: line.indent_columns,
                confidence: TextConfidence::High,
                kind: TextLineKind::Blank,
            });
            continue;
        }

        let next_nonblank = lines[index + 1..].iter().find(|item| !item.is_blank);
        let previous_blank = index == 0 || lines[index - 1].is_blank;
        let next_blank = index + 1 == lines.len() || lines[index + 1].is_blank;
        let indent_level = indent_level_for(line.indent_columns, &indent_levels, options);
        let marker = parse_marker(&line.text);

        let (kind, confidence) = match marker {
            Some(Marker::MarkdownHeading(level, _)) => {
                let level = level.min(6);
                (
                    TextLineKind::Heading {
                        level,
                        inferred: false,
                    },
                    TextConfidence::High,
                )
            }
            Some(Marker::Chapter { level: base, .. }) => {
                let inferred_level = if line.indent_columns > 0 {
                    base.saturating_add(indent_level.saturating_sub(1))
                } else {
                    base
                };
                (
                    TextLineKind::Heading {
                        level: inferred_level.clamp(1, options.max_inferred_depth),
                        inferred: false,
                    },
                    TextConfidence::High,
                )
            }
            Some(Marker::Numbered {
                depth,
                number,
                ref rest,
            }) => {
                let sequence = is_ordered_sequence(&lines, index, number);
                let heading_shape = is_heading_shape(rest, options.short_line_max_chars);
                if depth > 1 || (!sequence && heading_shape) {
                    (
                        TextLineKind::Heading {
                            level: u8::try_from(depth)
                                .unwrap_or(u8::MAX)
                                .clamp(1, options.max_inferred_depth),
                            inferred: false,
                        },
                        TextConfidence::High,
                    )
                } else {
                    (
                        TextLineKind::ListItem {
                            level: indent_level.max(1).min(options.max_inferred_depth),
                            ordered: true,
                        },
                        TextConfidence::High,
                    )
                }
            }
            Some(Marker::ChineseNumbered { number, ref rest }) => {
                let sequence = is_chinese_ordered_sequence(&lines, index, &number);
                if !sequence && is_heading_shape(rest, options.short_line_max_chars) {
                    (
                        TextLineKind::Heading {
                            level: indent_level.max(1).min(options.max_inferred_depth),
                            inferred: false,
                        },
                        TextConfidence::High,
                    )
                } else {
                    (
                        TextLineKind::ListItem {
                            level: indent_level.max(1).min(options.max_inferred_depth),
                            ordered: true,
                        },
                        TextConfidence::High,
                    )
                }
            }
            Some(Marker::ParenthesizedNumber { .. }) | Some(Marker::Bullet(_)) => (
                TextLineKind::ListItem {
                    level: indent_level.max(1).min(options.max_inferred_depth),
                    ordered: matches!(marker, Some(Marker::ParenthesizedNumber { .. })),
                },
                TextConfidence::High,
            ),
            None => {
                let strong_heading = strategy == TextStrategy::SmartLayering
                    && previous_blank
                    && (next_blank
                        || next_nonblank
                            .is_some_and(|next| next.indent_columns > line.indent_columns))
                    && is_heading_shape(&line.text, options.short_line_max_chars);
                if strong_heading {
                    (
                        TextLineKind::Heading {
                            level: indent_level.max(1).min(options.max_inferred_depth),
                            inferred: true,
                        },
                        TextConfidence::Medium,
                    )
                } else if strategy == TextStrategy::SmartLayering
                    && looks_like_ambiguous_short_heading(
                        &line.text,
                        previous_blank,
                        next_blank,
                        options.short_line_max_chars,
                    )
                {
                    (TextLineKind::ShortLineCandidate, TextConfidence::Low)
                } else {
                    (TextLineKind::Paragraph, TextConfidence::High)
                }
            }
        };

        match &kind {
            TextLineKind::Heading { level, inferred } => {
                heading_count += 1;
                if *inferred {
                    inferred_heading_count += 1;
                }
                max_depth = max_depth.max(*level);
            }
            TextLineKind::ListItem { level, .. } => {
                list_item_count += 1;
                max_depth = max_depth.max(*level);
            }
            TextLineKind::Paragraph => paragraph_line_count += 1,
            TextLineKind::ShortLineCandidate => {
                paragraph_line_count += 1;
                ambiguous_short_line_count += 1;
            }
            TextLineKind::Blank => {}
        }

        result.push(TextLineAnalysis {
            line_number: line.line_number,
            text: line.text.clone(),
            indent_columns: line.indent_columns,
            confidence,
            kind,
        });
    }

    let structure_confidence = if mixed_indentation {
        TextConfidence::Medium
    } else if heading_count > 0 || list_item_count > 0 {
        if inferred_heading_count > 0 {
            TextConfidence::Medium
        } else {
            TextConfidence::High
        }
    } else {
        TextConfidence::Low
    };

    TextAnalysis {
        strategy,
        lines: result,
        heading_count,
        inferred_heading_count,
        list_item_count,
        paragraph_line_count,
        ambiguous_short_line_count,
        mixed_indentation,
        max_depth: max_depth.min(options.max_inferred_depth),
        structure_confidence,
    }
}

fn strategy_for_preset(preset: &ConversionPreset) -> TextStrategy {
    match preset {
        ConversionPreset::StrictOutline => TextStrategy::StrictOutline,
        ConversionPreset::SentenceSplit => TextStrategy::SentenceSplit,
        ConversionPreset::StructureFirst | ConversionPreset::ExpandedContent => {
            TextStrategy::SmartLayering
        }
    }
}

fn indentation_levels(lines: &[RawLine]) -> Vec<usize> {
    let mut widths = lines
        .iter()
        .filter(|line| {
            !line.is_blank
                && line.indent_columns > 0
                && matches!(
                    parse_marker(&line.text),
                    Some(
                        Marker::Numbered { .. }
                            | Marker::ChineseNumbered { .. }
                            | Marker::ParenthesizedNumber { .. }
                            | Marker::Bullet(_)
                    )
                )
        })
        .map(|line| line.indent_columns)
        .collect::<Vec<_>>();
    widths.sort_unstable();
    widths.dedup();
    widths
}

fn indent_level_for(width: usize, levels: &[usize], options: &TextAdapterOptions) -> u8 {
    if width == 0 {
        return 1;
    }
    let rank = levels
        .iter()
        .take_while(|candidate| **candidate <= width)
        .count();
    u8::try_from(rank)
        .unwrap_or(u8::MAX)
        .max(1)
        .min(options.max_inferred_depth)
}

#[derive(Clone, Debug)]
enum Marker {
    MarkdownHeading(u8, String),
    Chapter {
        level: u8,
        title: String,
    },
    Numbered {
        depth: usize,
        number: u64,
        rest: String,
    },
    ChineseNumbered {
        number: String,
        rest: String,
    },
    ParenthesizedNumber {
        rest: String,
    },
    Bullet(String),
}

fn parse_marker(text: &str) -> Option<Marker> {
    if let Some(captures) = markdown_heading_regex().captures(text) {
        let level = captures.get(1)?.as_str().len() as u8;
        return Some(Marker::MarkdownHeading(
            level,
            captures.get(2)?.as_str().trim().to_string(),
        ));
    }
    if let Some(captures) = chapter_regex().captures(text) {
        let marker = captures.get(2)?.as_str();
        let level = if marker == "节" { 2 } else { 1 };
        return Some(Marker::Chapter {
            level,
            title: captures.get(3)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = english_chapter_regex().captures(text) {
        return Some(Marker::Chapter {
            level: 1,
            title: captures.get(2)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = nested_number_regex().captures(text) {
        let marker = captures.get(1)?.as_str();
        let depth = marker.split('.').count();
        return Some(Marker::Numbered {
            depth,
            number: marker
                .split('.')
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            rest: captures.get(2)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = simple_number_regex().captures(text) {
        return Some(Marker::Numbered {
            depth: 1,
            number: captures.get(1)?.as_str().parse().unwrap_or(0),
            rest: captures.get(2)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = chinese_number_regex().captures(text) {
        return Some(Marker::ChineseNumbered {
            number: captures.get(1)?.as_str().to_string(),
            rest: captures.get(2)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = parenthesized_number_regex().captures(text) {
        return Some(Marker::ParenthesizedNumber {
            rest: captures.get(2)?.as_str().trim().to_string(),
        });
    }
    if let Some(captures) = bullet_regex().captures(text) {
        return Some(Marker::Bullet(captures.get(1)?.as_str().trim().to_string()));
    }
    None
}

fn marker_title(marker: Marker) -> String {
    match marker {
        Marker::MarkdownHeading(_, title)
        | Marker::Chapter { title, .. }
        | Marker::Numbered { rest: title, .. }
        | Marker::ChineseNumbered { rest: title, .. }
        | Marker::ParenthesizedNumber { rest: title, .. }
        | Marker::Bullet(title) => title,
    }
}

fn is_ordered_sequence(lines: &[RawLine], index: usize, number: u64) -> bool {
    if index + 1 >= lines.len() || lines[index + 1].is_blank {
        return false;
    }
    let next = &lines[index + 1];
    if next.indent_columns != lines[index].indent_columns {
        return false;
    }
    matches!(
        parse_marker(&next.text),
        Some(Marker::Numbered {
            depth: 1,
            number: next_number,
            ..
        }) if next_number == number.saturating_add(1)
    )
}

fn is_chinese_ordered_sequence(lines: &[RawLine], index: usize, number: &str) -> bool {
    if index + 1 >= lines.len() || lines[index + 1].is_blank {
        return false;
    }
    let next = &lines[index + 1];
    if next.indent_columns != lines[index].indent_columns {
        return false;
    }
    matches!(
        parse_marker(&next.text),
        Some(Marker::ChineseNumbered { number: next_number, .. })
            if next_number != number
    )
}

fn is_heading_shape(text: &str, max_chars: usize) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max_chars {
        return false;
    }
    if trimmed.contains("://") || trimmed.starts_with("www.") {
        return false;
    }
    if trimmed.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    !trimmed.chars().last().is_some_and(|character| {
        matches!(
            character,
            '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';' | ',' | '，' | '、'
        )
    })
}

fn looks_like_ambiguous_short_heading(
    text: &str,
    previous_blank: bool,
    next_blank: bool,
    max_chars: usize,
) -> bool {
    (previous_blank || next_blank)
        && text.chars().count() <= max_chars
        && !text.contains("://")
        && !text.chars().last().is_some_and(|character| {
            matches!(
                character,
                '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';' | ',' | '，' | '、'
            )
        })
}

fn build_document(
    source: SourceInput<'_>,
    decoded: DecodedText,
    analysis: TextAnalysis,
    options: &ConversionOptions,
    adapter_options: &TextAdapterOptions,
) -> Result<OutlineDocument, ImportError> {
    let title = source_title(source.path, "Untitled text");
    let mut provenance = SourceProvenance::new(SourceFormat::Text, ADAPTER_NAME, ADAPTER_VERSION);
    provenance.source_path = source.path.and_then(Path::to_str).map(ToString::to_string);
    provenance.source_sha256 = format!("{:x}", Sha256::digest(source.bytes));
    provenance
        .original_format
        .insert("encoding".to_string(), json!(decoded.encoding));
    provenance
        .original_format
        .insert("strategy".to_string(), json!(analysis.strategy));
    provenance.original_format.insert(
        "structure_confidence".to_string(),
        json!(analysis.structure_confidence),
    );
    provenance.original_format.insert(
        "mixed_indentation".to_string(),
        json!(analysis.mixed_indentation),
    );
    provenance.original_format.insert(
        "inferred_heading_count".to_string(),
        json!(analysis.inferred_heading_count),
    );
    provenance.original_format.insert(
        "ambiguous_short_line_count".to_string(),
        json!(analysis.ambiguous_short_line_count),
    );
    provenance.original_format.insert(
        "max_inferred_depth".to_string(),
        json!(adapter_options.max_inferred_depth),
    );

    let mut document = OutlineDocument::new(title, provenance);
    if analysis.mixed_indentation {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::InvalidStructure,
            "TXT mixes tab and space indentation; inferred levels use normalized widths",
            None,
        ));
    }
    if analysis.ambiguous_short_line_count > 0 {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::InvalidStructure,
            format!(
                "{} short line(s) looked like headings but lacked enough blank-line context",
                analysis.ambiguous_short_line_count
            ),
            None,
        ));
    }

    let mut builder = OutlineTreeBuilder::new();
    let mut pending_paragraph = Vec::<String>::new();
    let mut pending_start_line = None::<u32>;
    let mut has_body_section = false;

    for line in &analysis.lines {
        match &line.kind {
            TextLineKind::Blank => flush_text_paragraph(
                &mut builder,
                &mut document,
                &mut pending_paragraph,
                &mut pending_start_line,
                options,
                &mut has_body_section,
            )?,
            TextLineKind::Heading { level, .. } => {
                flush_text_paragraph(
                    &mut builder,
                    &mut document,
                    &mut pending_paragraph,
                    &mut pending_start_line,
                    options,
                    &mut has_body_section,
                )?;
                let marker = parse_marker(&line.text);
                let raw_title = marker
                    .map(marker_title)
                    .unwrap_or_else(|| line.text.clone());
                let (title, truncated) = truncate_text(&raw_title, options.max_title_chars);
                if truncated {
                    document.stats.truncated_titles += 1;
                    document.warnings.push(ConversionWarning::warning(
                        ConversionWarningCode::TitleTruncated,
                        "TXT heading title was truncated",
                        Some(text_source_location(line)),
                    ));
                }
                let node = OutlineNode::structure(
                    if title.is_empty() {
                        "Untitled heading"
                    } else {
                        &title
                    },
                    StructureKind::Heading { level: *level },
                )
                .with_source(text_source_location(line));
                builder.append_heading((*level).min(options.max_depth), node);
            }
            TextLineKind::ListItem { level, ordered } => {
                flush_text_paragraph(
                    &mut builder,
                    &mut document,
                    &mut pending_paragraph,
                    &mut pending_start_line,
                    options,
                    &mut has_body_section,
                )?;
                let marker = parse_marker(&line.text);
                let raw_title = marker
                    .map(marker_title)
                    .unwrap_or_else(|| line.text.clone());
                let (title, truncated) = truncate_text(&raw_title, options.max_title_chars);
                if truncated {
                    document.stats.truncated_titles += 1;
                    document.warnings.push(ConversionWarning::warning(
                        ConversionWarningCode::TitleTruncated,
                        "TXT list item was truncated",
                        Some(text_source_location(line)),
                    ));
                }
                let mut node = OutlineNode::structure(
                    if title.is_empty() {
                        "Untitled item"
                    } else {
                        &title
                    },
                    StructureKind::ListItem {
                        ordered: *ordered,
                        level: *level,
                    },
                )
                .with_source(text_source_location(line));
                if options.preserve_original_paragraphs {
                    node.notes.push(OutlineNote::with_label(
                        &line.text,
                        "Original list item",
                        Some(text_source_location(line)),
                    ));
                }
                let path = builder.append_list_item(
                    (*level).min(options.max_depth),
                    node,
                    options.max_depth,
                );
                if options.split_content && !title.is_empty() {
                    promote_first_sentence(
                        &mut builder,
                        path,
                        &raw_title,
                        line,
                        options,
                        &mut document,
                    )?;
                }
            }
            TextLineKind::Paragraph | TextLineKind::ShortLineCandidate => {
                if pending_start_line.is_none() {
                    pending_start_line = Some(line.line_number);
                }
                pending_paragraph.push(line.text.clone());
            }
        }
    }

    flush_text_paragraph(
        &mut builder,
        &mut document,
        &mut pending_paragraph,
        &mut pending_start_line,
        options,
        &mut has_body_section,
    )?;
    document.children = builder.finish();

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
    if document.node_count() == 0 {
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::EmptyInput,
            "TXT contained no convertible text",
            None,
        ));
    }
    Ok(document)
}

fn flush_text_paragraph(
    builder: &mut OutlineTreeBuilder,
    document: &mut OutlineDocument,
    pending: &mut Vec<String>,
    start_line: &mut Option<u32>,
    options: &ConversionOptions,
    has_body_section: &mut bool,
) -> Result<(), ImportError> {
    if pending.is_empty() {
        return Ok(());
    }
    builder.close_list();
    if !*has_body_section && builder.current_path().is_empty() {
        builder.ensure_body_section("Body");
        *has_body_section = true;
    }
    let paragraph = pending.join("\n");
    pending.clear();
    let line = start_line.take().unwrap_or(1);
    let source = SourceLocation {
        path: Vec::new(),
        line: Some(line),
        column: None,
        byte_offset: None,
    };
    append_body_paragraph(builder, document, &paragraph, source, options)
}

fn append_body_paragraph(
    builder: &mut OutlineTreeBuilder,
    document: &mut OutlineDocument,
    paragraph: &str,
    source: SourceLocation,
    options: &ConversionOptions,
) -> Result<(), ImportError> {
    let parent = builder
        .current_node_mut()
        .expect("body section must be selected before appending text");
    if !options.split_content {
        let (title, truncated) = truncate_text(paragraph, options.max_title_chars);
        if truncated {
            document.stats.truncated_titles += 1;
            document.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::TitleTruncated,
                "TXT paragraph node was truncated",
                Some(source.clone()),
            ));
        }
        let mut node = OutlineNode::content(
            if title.is_empty() {
                "Untitled paragraph"
            } else {
                &title
            },
            ContentKind::Paragraph,
        )
        .with_source(source.clone());
        if options.preserve_original_paragraphs {
            node.notes.push(OutlineNote::with_label(
                paragraph,
                "Original paragraph",
                Some(source),
            ));
        }
        parent.children.push(node);
        return Ok(());
    }

    let split_options = SentenceSplitOptions::from(options);
    let fragments = segment_sentences(paragraph, &split_options);
    if fragments.len() <= LONG_PARAGRAPH_THRESHOLD {
        let report = add_paragraph_as_content_nodes(parent, paragraph, Some(source), options)?;
        record_expansion(document, report);
        return Ok(());
    }

    let mut group_options = options.clone();
    group_options.preserve_original_paragraphs = true;
    if options.preserve_original_paragraphs {
        parent.notes.push(OutlineNote::with_label(
            paragraph,
            "Original long paragraph",
            Some(source.clone()),
        ));
    }
    for (group_index, chunk) in paragraph_chunks(paragraph, PARAGRAPH_GROUP_SIZE)
        .into_iter()
        .enumerate()
    {
        let group_title = format!("Paragraph {}", group_index + 1);
        let mut group =
            OutlineNode::content(group_title, ContentKind::Paragraph).with_source(source.clone());
        group.notes.push(OutlineNote::with_label(
            chunk.clone(),
            format!("Paragraph group {}", group_index + 1),
            Some(source.clone()),
        ));
        let before = parent.children.len();
        parent.children.push(group);
        let group = parent
            .children
            .get_mut(before)
            .expect("new paragraph group must exist");
        let nested_options = ConversionOptions {
            preserve_original_paragraphs: false,
            ..group_options.clone()
        };
        let report =
            add_paragraph_as_content_nodes(group, &chunk, Some(source.clone()), &nested_options)?;
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
    Ok(())
}

fn paragraph_chunks(paragraph: &str, fragment_group_size: usize) -> Vec<String> {
    let options = SentenceSplitOptions {
        min_fragment_chars: 1,
        merge_short_fragments: false,
    };
    let fragments = segment_sentences(paragraph, &options);
    if fragments.is_empty() {
        return Vec::new();
    }
    fragments
        .chunks(fragment_group_size.max(1))
        .map(|chunk| {
            let start = chunk.first().map(|fragment| fragment.start).unwrap_or(0);
            let end = chunk.last().map(|fragment| fragment.end).unwrap_or(0);
            paragraph[start..end].trim().to_string()
        })
        .collect()
}

fn promote_first_sentence(
    builder: &mut OutlineTreeBuilder,
    path: Vec<usize>,
    text: &str,
    line: &TextLineAnalysis,
    options: &ConversionOptions,
    document: &mut OutlineDocument,
) -> Result<(), ImportError> {
    let split_options = SentenceSplitOptions {
        min_fragment_chars: options.min_fragment_chars,
        merge_short_fragments: options.merge_short_fragments,
    };
    let fragments = segment_sentences(text, &split_options);
    if fragments.len() < 2 {
        return Ok(());
    }
    let Some(node) = builder.node_mut(&path) else {
        return Ok(());
    };
    let (title, truncated) = truncate_text(&fragments[0].text, options.max_title_chars);
    node.title = if title.is_empty() {
        "Untitled item".to_string()
    } else {
        title
    };
    if truncated {
        document.stats.truncated_titles += 1;
        document.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::TitleTruncated,
            "TXT list item title was truncated",
            Some(text_source_location(line)),
        ));
    }
    for fragment in fragments.iter().skip(1) {
        let (fragment_title, fragment_truncated) =
            truncate_text(&fragment.text, options.max_title_chars);
        if fragment_truncated {
            document.stats.truncated_titles += 1;
        }
        let child = OutlineNode {
            title: if fragment_title.is_empty() {
                "Untitled fragment".to_string()
            } else {
                fragment_title
            },
            kind: OutlineNodeKind::Content {
                kind: ContentKind::Sentence,
                split_from_paragraph: true,
            },
            children: Vec::new(),
            notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            tables: Vec::new(),
            task: None,
            source: Some(text_source_location(line)),
        };
        node.children.push(child);
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

fn text_source_location(line: &TextLineAnalysis) -> SourceLocation {
    SourceLocation {
        path: Vec::new(),
        line: Some(line.line_number),
        column: None,
        byte_offset: None,
    }
}

fn source_title(path: Option<&Path>, fallback: &str) -> String {
    path.and_then(Path::file_stem)
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn markdown_heading_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^(#{1,6})\s+(.+)$").expect("valid Markdown heading regex"))
}

fn chapter_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^第\s*([零〇一二三四五六七八九十百千万两\d]+)\s*([章节篇部])\s*[：:、.\-]?\s*(.*)$",
        )
        .expect("valid Chinese chapter regex")
    })
}

fn english_chapter_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^chapter\s+([0-9ivxlcdm]+)\b[\s:.\-]*(.*)$")
            .expect("valid English chapter regex")
    })
}

fn nested_number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(\d+(?:\.\d+)+)[.)、]?\s+(.+)$").expect("valid nested numbering regex")
    })
}

fn simple_number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^(\d+)[.)、]\s*(.+)$").expect("valid simple numbering regex"))
}

fn chinese_number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^([零〇一二三四五六七八九十百千万两]+)[、.)）]\s*(.+)$")
            .expect("valid Chinese numbering regex")
    })
}

fn parenthesized_number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^[(（](\d+)[)）]\s*(.+)$").expect("valid parenthesized numbering regex")
    })
}

fn bullet_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^[-*+•·◦▪▫‣]\s*(.+)$").expect("valid bullet regex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_explicit_structure_without_deep_unstructured_tree() {
        let analysis = analyze_decoded_text(
            "第一章 总览\n\n1. 架构\n  - Rust Core\n\n正文没有显式结构。下一句在这里。",
            &ConversionPreset::StructureFirst,
            &TextAdapterOptions::default(),
        );
        assert!(analysis.heading_count >= 2);
        assert!(analysis.list_item_count >= 1);
        assert!(analysis.max_depth <= 4);
    }

    #[test]
    fn keeps_short_sentence_as_paragraph_without_enough_context() {
        let analysis = analyze_decoded_text(
            "这是普通的一段。它并没有标题。\n下一句继续。",
            &ConversionPreset::StructureFirst,
            &TextAdapterOptions::default(),
        );
        assert_eq!(analysis.heading_count, 0);
        assert_eq!(analysis.paragraph_line_count, 2);
    }
}
