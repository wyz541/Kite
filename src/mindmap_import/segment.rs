//! Deterministic paragraph segmentation used by all import adapters.

use crate::mindmap_import::model::{
    ContentKind, ConversionOptions, ConversionWarning, ConversionWarningCode, ImportError,
    OutlineNode, OutlineNodeKind, OutlineNote, SourceLocation, truncate_text,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceSplitOptions {
    pub min_fragment_chars: usize,
    pub merge_short_fragments: bool,
}

impl Default for SentenceSplitOptions {
    fn default() -> Self {
        Self {
            min_fragment_chars: ConversionOptions::default().min_fragment_chars,
            merge_short_fragments: true,
        }
    }
}

impl From<&ConversionOptions> for SentenceSplitOptions {
    fn from(options: &ConversionOptions) -> Self {
        Self {
            min_fragment_chars: options.min_fragment_chars,
            merge_short_fragments: options.merge_short_fragments,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFragment {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParagraphExpansionReport {
    pub nodes_added: usize,
    pub merged_fragments: usize,
    pub original_preserved: bool,
    pub warnings: Vec<ConversionWarning>,
}

/// Split a paragraph at Chinese and English sentence punctuation while
/// protecting URLs, decimal/version dots, and punctuation inside quotes or
/// brackets. The returned byte ranges are stable slices of the original input.
pub fn segment_sentences(text: &str, options: &SentenceSplitOptions) -> Vec<TextFragment> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let mut protected = vec![false; text.len()];
    mark_url_ranges(text, &mut protected);
    mark_decimal_and_version_dots(text, &mut protected);

    let mut raw = Vec::new();
    let mut bracket_stack = Vec::<char>::new();
    let mut double_quote_open = false;
    let mut single_quote_open = false;
    let mut segment_start = 0;

    for (index, character) in text.char_indices() {
        update_quote_and_bracket_state(
            text,
            index,
            character,
            &mut bracket_stack,
            &mut double_quote_open,
            &mut single_quote_open,
        );

        let inside_context = !bracket_stack.is_empty() || double_quote_open || single_quote_open;
        if !is_sentence_terminator(character)
            || inside_context
            || protected.get(index).copied().unwrap_or(false)
        {
            continue;
        }

        let mut boundary_end = index + character.len_utf8();
        let remainder = &text[boundary_end..];
        for following in remainder.chars() {
            if is_sentence_terminator(following) {
                boundary_end += following.len_utf8();
                continue;
            }
            break;
        }

        push_trimmed_range(text, segment_start, boundary_end, &mut raw);
        segment_start = boundary_end;
    }

    push_trimmed_range(text, segment_start, text.len(), &mut raw);
    merge_short_fragments(text, raw, options)
}

/// Add content nodes for one paragraph and preserve the complete paragraph as
/// a note on the nearest structure node. Warnings are returned rather than
/// silently mutating the document report.
pub fn add_paragraph_as_content_nodes(
    parent: &mut OutlineNode,
    paragraph: &str,
    source: Option<SourceLocation>,
    options: &ConversionOptions,
) -> Result<ParagraphExpansionReport, ImportError> {
    options.validate()?;

    let trimmed = paragraph.trim();
    if trimmed.is_empty() {
        return Ok(ParagraphExpansionReport {
            warnings: vec![ConversionWarning::warning(
                ConversionWarningCode::EmptyInput,
                "Skipped an empty paragraph",
                source,
            )],
            ..ParagraphExpansionReport::default()
        });
    }

    let mut report = ParagraphExpansionReport::default();
    if options.preserve_original_paragraphs {
        parent.notes.push(OutlineNote::with_label(
            trimmed,
            "Original paragraph",
            source.clone(),
        ));
        report.original_preserved = true;
    }

    if !options.split_content {
        report.warnings.push(ConversionWarning::warning(
            ConversionWarningCode::ContentSkipped,
            "Paragraph content was preserved in notes because content splitting is disabled",
            source,
        ));
        return Ok(report);
    }

    let split_options = SentenceSplitOptions::from(options);
    let fragments = segment_sentences(trimmed, &split_options);
    let raw_count = count_raw_fragments(trimmed, &split_options);
    report.merged_fragments = raw_count.saturating_sub(fragments.len());

    for fragment in fragments {
        if fragment.text.is_empty() {
            continue;
        }
        let fragment_source = source.clone().map(|mut location| {
            if let Some(offset) = location.byte_offset.as_mut() {
                *offset = offset.saturating_add(fragment.start);
            }
            location
        });
        let (title, was_truncated) = truncate_text(&fragment.text, options.max_title_chars);
        if was_truncated {
            report.warnings.push(ConversionWarning::warning(
                ConversionWarningCode::TitleTruncated,
                format!(
                    "Paragraph fragment was truncated to {} characters",
                    options.max_title_chars
                ),
                fragment_source.clone(),
            ));
        }
        let mut node = OutlineNode {
            title,
            kind: OutlineNodeKind::Content {
                kind: ContentKind::Sentence,
                split_from_paragraph: true,
            },
            children: Vec::new(),
            notes: Vec::new(),
            links: Vec::new(),
            attachments: Vec::new(),
            tables: Vec::new(),
            source: fragment_source,
        };
        if node.title.is_empty() {
            node.title = "Untitled fragment".to_string();
        }
        parent.children.push(node);
        report.nodes_added += 1;
    }

    Ok(report)
}

fn count_raw_fragments(text: &str, options: &SentenceSplitOptions) -> usize {
    let raw_options = SentenceSplitOptions {
        merge_short_fragments: false,
        ..options.clone()
    };
    segment_sentences(text, &raw_options).len()
}

fn merge_short_fragments(
    source: &str,
    fragments: Vec<TextFragment>,
    options: &SentenceSplitOptions,
) -> Vec<TextFragment> {
    if !options.merge_short_fragments || options.min_fragment_chars == 0 || fragments.len() < 2 {
        return fragments;
    }

    let mut merged = Vec::<TextFragment>::with_capacity(fragments.len());
    for fragment in fragments {
        if significant_char_count(&fragment.text) < options.min_fragment_chars
            && let Some(previous) = merged.last_mut()
        {
            previous.end = fragment.end;
            previous.text = source[previous.start..previous.end].trim().to_string();
            continue;
        }
        merged.push(fragment);
    }

    while merged.len() > 1 && significant_char_count(&merged[0].text) < options.min_fragment_chars {
        let next = merged.remove(1);
        merged[0].end = next.end;
        merged[0].text = source[merged[0].start..merged[0].end].trim().to_string();
    }

    merged
}

fn significant_char_count(text: &str) -> usize {
    text.chars()
        .filter(|character| !character.is_whitespace() && !is_sentence_punctuation(*character))
        .count()
}

fn push_trimmed_range(source: &str, start: usize, end: usize, output: &mut Vec<TextFragment>) {
    let mut trimmed_start = start;
    while trimmed_start < end {
        let character = source[trimmed_start..]
            .chars()
            .next()
            .expect("start is before end");
        if !character.is_whitespace() {
            break;
        }
        trimmed_start += character.len_utf8();
    }

    let mut trimmed_end = end;
    while trimmed_end > trimmed_start {
        let character = source[..trimmed_end]
            .chars()
            .next_back()
            .expect("end is after start");
        if !character.is_whitespace() {
            break;
        }
        trimmed_end -= character.len_utf8();
    }

    if trimmed_start < trimmed_end {
        output.push(TextFragment {
            text: source[trimmed_start..trimmed_end].to_string(),
            start: trimmed_start,
            end: trimmed_end,
        });
    }
}

fn mark_url_ranges(text: &str, protected: &mut [bool]) {
    let mut offset = 0;
    while offset < text.len() {
        if !starts_url(text, offset) {
            let character = text[offset..].chars().next().expect("valid offset");
            offset += character.len_utf8();
            continue;
        }

        let mut token_end = offset;
        let mut last_non_punctuation_end = offset;
        for (relative, character) in text[offset..].char_indices() {
            if character.is_whitespace() || is_url_hard_delimiter(character) {
                break;
            }
            token_end = offset + relative + character.len_utf8();
            if !is_url_trailing_punctuation(character) {
                last_non_punctuation_end = token_end;
            }
        }

        let protected_end = last_non_punctuation_end.max(offset);
        for value in &mut protected[offset..protected_end] {
            *value = true;
        }
        offset = token_end.max(offset + 1);
    }
}

fn starts_url(text: &str, offset: usize) -> bool {
    let remainder = &text[offset..];
    [
        "https://", "http://", "ftp://", "file://", "mailto:", "www.",
    ]
    .iter()
    .any(|prefix| {
        remainder
            .as_bytes()
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix.as_bytes()))
    })
}

fn is_url_hard_delimiter(character: char) -> bool {
    matches!(
        character,
        '。' | '！'
            | '？'
            | '；'
            | '，'
            | '：'
            | '、'
            | '（'
            | '）'
            | '【'
            | '】'
            | '《'
            | '》'
            | '「'
            | '」'
            | '『'
            | '』'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

fn is_url_trailing_punctuation(character: char) -> bool {
    matches!(
        character,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '”' | '’'
    )
}

fn mark_decimal_and_version_dots(text: &str, protected: &mut [bool]) {
    for (index, character) in text.char_indices() {
        if character != '.' {
            continue;
        }
        let previous = text[..index].chars().next_back();
        let next = text[index + 1..].chars().next();
        if previous.is_some_and(|value| value.is_ascii_digit())
            && next.is_some_and(|value| value.is_ascii_digit())
        {
            protected[index] = true;
        }
    }
}

fn update_quote_and_bracket_state(
    text: &str,
    index: usize,
    character: char,
    bracket_stack: &mut Vec<char>,
    double_quote_open: &mut bool,
    single_quote_open: &mut bool,
) {
    if let Some(closing) = closing_bracket(character) {
        bracket_stack.push(closing);
        return;
    }
    if bracket_stack.last().copied() == Some(character) {
        bracket_stack.pop();
        return;
    }

    match character {
        '“' => *double_quote_open = true,
        '”' => *double_quote_open = false,
        '"' => *double_quote_open = !*double_quote_open,
        '‘' => *single_quote_open = true,
        '’' => *single_quote_open = false,
        '\'' if !is_word_internal_apostrophe(text, index) => {
            *single_quote_open = !*single_quote_open;
        }
        _ => {}
    }
}

fn closing_bracket(character: char) -> Option<char> {
    match character {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '（' => Some('）'),
        '【' => Some('】'),
        '《' => Some('》'),
        '〈' => Some('〉'),
        '「' => Some('」'),
        '『' => Some('』'),
        _ => None,
    }
}

fn is_word_internal_apostrophe(text: &str, index: usize) -> bool {
    let previous = text[..index].chars().next_back();
    let next = text[index + 1..].chars().next();
    previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric)
}

fn is_sentence_terminator(character: char) -> bool {
    matches!(character, '。' | '！' | '？' | '；' | '.' | '?' | '!' | ';')
}

fn is_sentence_punctuation(character: char) -> bool {
    is_sentence_terminator(character)
        || matches!(
            character,
            ',' | '，' | '、' | ':' | '：' | ')' | '）' | ']' | '】' | '}' | '”' | '’'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(fragments: &[TextFragment]) -> Vec<&str> {
        fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect()
    }

    #[test]
    fn splits_chinese_and_english_punctuation() {
        let fragments = segment_sentences(
            "第一句。第二句！第三句？第四句；Fifth. Sixth? Seventh! Eighth;",
            &SentenceSplitOptions {
                min_fragment_chars: 1,
                merge_short_fragments: false,
            },
        );

        assert_eq!(
            texts(&fragments),
            vec![
                "第一句。",
                "第二句！",
                "第三句？",
                "第四句；",
                "Fifth.",
                "Sixth?",
                "Seventh!",
                "Eighth;",
            ]
        );
    }

    #[test]
    fn protects_urls_decimals_versions_quotes_and_brackets() {
        let input = "访问 https://example.com/a.b?x=1.2。版本 v1.2.3 已发布。他说：“先不要拆。这里仍在引号内。”然后离开。(Note: keep this. Inside.)完成！";
        let fragments = segment_sentences(
            input,
            &SentenceSplitOptions {
                min_fragment_chars: 1,
                merge_short_fragments: false,
            },
        );

        assert_eq!(fragments.len(), 4, "{:?}", texts(&fragments));
        assert!(fragments[0].text.contains("https://example.com/a.b?x=1.2"));
        assert!(fragments[1].text.contains("v1.2.3"));
        assert!(fragments[2].text.contains("先不要拆。这里仍在引号内。"));
        assert!(fragments[3].text.contains("Note: keep this. Inside."));
    }

    #[test]
    fn preserves_trailing_period_after_url_as_sentence_boundary() {
        let fragments = segment_sentences(
            "Read https://example.com/docs. Then continue.",
            &SentenceSplitOptions {
                min_fragment_chars: 1,
                merge_short_fragments: false,
            },
        );

        assert_eq!(
            texts(&fragments),
            vec!["Read https://example.com/docs.", "Then continue."]
        );
    }

    #[test]
    fn merges_short_fragments_with_an_adjacent_sentence() {
        let input = "OK. This sentence is already long enough.";
        let merged = segment_sentences(
            input,
            &SentenceSplitOptions {
                min_fragment_chars: 12,
                merge_short_fragments: true,
            },
        );
        assert_eq!(texts(&merged), vec![input]);

        let unmerged = segment_sentences(
            input,
            &SentenceSplitOptions {
                min_fragment_chars: 12,
                merge_short_fragments: false,
            },
        );
        assert_eq!(
            texts(&unmerged),
            vec!["OK.", "This sentence is already long enough."]
        );
    }
}
