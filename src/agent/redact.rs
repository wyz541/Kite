use std::cmp::Reverse;
use std::sync::Arc;

use regex::Regex;

use crate::agent::credentials::MIN_CREDENTIAL_LENGTH;

const SENSITIVE_MARKERS: &[&str] = &[
    "bearer",
    "sk-",
    "token",
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "credential",
];

#[derive(Debug)]
pub(crate) struct Redactor {
    regexes: Vec<Regex>,
    secrets: Vec<String>,
}

impl Redactor {
    pub(crate) fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|secret| secret.chars().count() >= MIN_CREDENTIAL_LENGTH);
        secrets.sort_by_key(|secret| Reverse(secret.len()));
        secrets.dedup();

        let patterns = [
            r"(?i)([a-z][a-z0-9+.-]*://)([^/\s:@]+):([^/\s@]+)@",
            r#"(?i)(["'](?:token|password|passwd|secret|api[_-]?key|authorization|credential)["']\s*:\s*)(?:"(?:\\.|[^"])*"|'(?:\\.|[^'])*'|[^,}\s]+)"#,
            r"(?i)\b(authorization\s*[:=]\s*(?:bearer\s+)?)[^\s,;]+",
            r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]+",
            r"(?i)\b(sk-)[A-Za-z0-9_-]+",
            r#"(?i)\b((?:token|password|passwd|secret|api[_-]?key|credential)\b\s*[:=]\s*)[^\s,;]+"#,
        ];

        Self {
            regexes: patterns
                .into_iter()
                .filter_map(|pattern| Regex::new(pattern).ok())
                .collect(),
            secrets,
        }
    }

    pub(crate) fn redact(&self, input: &str) -> String {
        let mut output = input.to_string();
        for secret in &self.secrets {
            output = output.replace(secret, "[REDACTED]");
        }
        for regex in &self.regexes {
            output = regex
                .replace_all(&output, |captures: &regex::Captures<'_>| {
                    if captures.len() >= 2 {
                        format!("{}[REDACTED]", &captures[1])
                    } else {
                        "[REDACTED]".to_string()
                    }
                })
                .into_owned();
        }
        output
    }

    fn pending_secret_prefix_bytes(&self, text: &str) -> usize {
        let Some(longest) = self.secrets.iter().map(String::len).max() else {
            return 0;
        };
        if longest <= 1 {
            return 0;
        }
        let mut start = text.len().saturating_sub(longest - 1);
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        for (offset, _) in text[start..].char_indices() {
            let boundary = start + offset;
            let suffix = &text[boundary..];
            if self
                .secrets
                .iter()
                .any(|secret| secret.len() > suffix.len() && secret.starts_with(suffix))
            {
                return suffix.len();
            }
        }
        0
    }

    fn pending_sensitive_prefix_bytes(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        let lower = text.to_ascii_lowercase();
        let mut hold_start = None;

        for marker in SENSITIVE_MARKERS {
            let partial = ascii_prefix_suffix_bytes(&lower, marker);
            if partial > 0 {
                hold_start = Some(hold_start.map_or(text.len() - partial, |start: usize| {
                    start.min(text.len() - partial)
                }));
            }

            let mut search_end = lower.len();
            while let Some(relative) = lower[..search_end].rfind(marker) {
                if marker_starts_at_boundary(&lower, relative)
                    && open_sensitive_tail_start(text, relative, marker).is_some()
                {
                    hold_start = Some(hold_start.map_or(relative, |start| start.min(relative)));
                    break;
                }
                search_end = relative;
            }
        }
        if let Some(url_start) = open_url_start(text) {
            hold_start = Some(hold_start.map_or(url_start, |start| start.min(url_start)));
        }

        hold_start.map_or(0, |start| text.len().saturating_sub(start))
    }

    fn redact_stream_tail(&self, input: &str) -> String {
        let hold = self.pending_secret_prefix_bytes(input);
        if hold == 0 {
            return self.redact(input);
        }
        let split = input.len().saturating_sub(hold);
        let mut output = self.redact(&input[..split]);
        output.push_str("[REDACTED]");
        output
    }

    pub(crate) fn truncate(&self, input: &str, max_chars: usize) -> String {
        let redacted = self.redact(input);
        let mut chars = redacted.chars();
        let prefix = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            format!("{prefix}…")
        } else {
            prefix
        }
    }
}

#[derive(Debug)]
pub(crate) struct StreamingRedactor {
    redactor: Arc<Redactor>,
    pending: String,
}

impl StreamingRedactor {
    pub(crate) fn new(redactor: Arc<Redactor>) -> Self {
        Self {
            redactor,
            pending: String::new(),
        }
    }

    /// Returns only safe output. A suffix that could become an exact secret is
    /// held until a later chunk resolves it or the caller flushes the stream.
    pub(crate) fn push(&mut self, input: &str) -> String {
        if input.is_empty() {
            return String::new();
        }
        self.pending.push_str(input);
        let hold = self
            .redactor
            .pending_secret_prefix_bytes(&self.pending)
            .max(self.redactor.pending_sensitive_prefix_bytes(&self.pending));
        let split = self.pending.len().saturating_sub(hold);
        if split == 0 {
            return String::new();
        }
        let ready = self.pending[..split].to_string();
        self.pending.drain(..split);
        self.redactor.redact(&ready)
    }

    pub(crate) fn finish(&mut self) -> String {
        let pending = std::mem::take(&mut self.pending);
        self.redactor.redact_stream_tail(&pending)
    }

    pub(crate) fn reset(&mut self) {
        self.pending.clear();
    }
}

fn ascii_prefix_suffix_bytes(text: &str, marker: &str) -> usize {
    let marker = marker.as_bytes();
    let max = text.len().min(marker.len().saturating_sub(1));
    for length in (1..=max).rev() {
        let start = text.len() - length;
        if text.is_char_boundary(start)
            && text.as_bytes()[start..].eq_ignore_ascii_case(&marker[..length])
        {
            return length;
        }
    }
    0
}

fn marker_starts_at_boundary(text: &str, start: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_none_or(|character| !is_word_character(character))
}

fn open_sensitive_tail_start(text: &str, start: usize, marker: &str) -> Option<usize> {
    let end = start + marker.len();
    match marker {
        "bearer" => open_bearer_tail_start(text, start, end),
        "sk-" => open_token_tail_start(text, start, end, is_sk_token_character),
        _ => open_assignment_tail_start(text, start, end),
    }
}

fn open_bearer_tail_start(text: &str, marker_start: usize, marker_end: usize) -> Option<usize> {
    let remainder = text.get(marker_end..)?;
    let Some(first) = remainder.chars().next() else {
        return Some(marker_start);
    };
    if !first.is_whitespace() {
        return None;
    }
    let token_start = marker_end + remainder.len() - remainder.trim_start().len();
    open_token_tail_start(text, marker_start, token_start, is_bearer_token_character)
}

fn open_token_tail_start(
    text: &str,
    marker_start: usize,
    token_start: usize,
    token_character: fn(char) -> bool,
) -> Option<usize> {
    if token_start >= text.len() {
        return Some(marker_start);
    }
    text[token_start..]
        .chars()
        .all(token_character)
        .then_some(marker_start)
}

fn open_assignment_tail_start(text: &str, marker_start: usize, marker_end: usize) -> Option<usize> {
    let mut cursor = marker_end;
    cursor = skip_ascii_whitespace(text, cursor);

    if let Some(character) = text[cursor..].chars().next()
        && matches!(character, '"' | '\'')
    {
        cursor += character.len_utf8();
        cursor = skip_ascii_whitespace(text, cursor);
    }

    if cursor >= text.len() {
        return Some(marker_start);
    }
    let separator = text[cursor..].chars().next()?;
    if !matches!(separator, ':' | '=') {
        return None;
    }
    cursor += separator.len_utf8();
    cursor = skip_ascii_whitespace(text, cursor);
    if cursor >= text.len() {
        return Some(marker_start);
    }

    let mut characters = text[cursor..].char_indices().peekable();
    let (_, first) = *characters.peek()?;
    if matches!(first, '"' | '\'') {
        let quote = first;
        let mut escaped = false;
        for (_, character) in characters.skip(1) {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                return None;
            }
        }
        Some(marker_start)
    } else {
        characters
            .all(|(_, character)| {
                !matches!(character, ',' | ';' | '}') && !character.is_whitespace()
            })
            .then_some(marker_start)
    }
}

fn skip_ascii_whitespace(text: &str, mut cursor: usize) -> usize {
    while let Some(character) = text[cursor..].chars().next() {
        if !character.is_whitespace() {
            break;
        }
        cursor += character.len_utf8();
    }
    cursor
}

fn open_url_start(text: &str) -> Option<usize> {
    if let Some(separator) = text.rfind("://") {
        let start = url_scheme_start(text, separator);
        return (!text[separator + 3..].chars().any(char::is_whitespace)).then_some(start);
    }
    let partial = ascii_prefix_suffix_bytes(text, "://");
    if partial == 0 {
        return None;
    }
    let scheme_end = text.len().saturating_sub(partial);
    Some(url_scheme_start(text, scheme_end))
}

fn url_scheme_start(text: &str, mut end: usize) -> usize {
    while end > 0 {
        let Some(character) = text[..end].chars().next_back() else {
            break;
        };
        if !(character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')) {
            break;
        }
        end -= character.len_utf8();
    }
    end
}

fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-')
}

fn is_bearer_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '.' | '_' | '~' | '+' | '/' | '=' | '-')
}

fn is_sk_token_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

pub(crate) fn is_sensitive_environment_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTHORIZATION",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_secret_environment_values_and_url_credentials() {
        let redactor = Redactor::new(vec!["swordfish-123".to_string()]);
        let text = "token=swordfish-123 url=https://alice:hunter2@example.test/x";
        let redacted = redactor.redact(text);
        assert!(!redacted.contains("swordfish-123"));
        assert!(!redacted.contains("hunter2"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn masks_exact_random_secret_without_relying_on_its_shape() {
        let redactor = Redactor::new(vec!["m4ng0-river-91".to_string()]);
        let redacted = redactor.redact("The value is m4ng0-river-91 in ordinary prose.");
        assert!(!redacted.contains("m4ng0-river-91"));
        assert!(redacted.contains("The value is [REDACTED] in ordinary prose."));
    }

    #[test]
    fn streaming_redactor_handles_a_secret_split_across_chunks() {
        let redactor = Arc::new(Redactor::new(vec!["random-secret-value".to_string()]));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("prefix random-se"), "prefix ");
        assert_eq!(stream.push("cret-value suffix"), "[REDACTED] suffix");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn streaming_redactor_flushes_ordinary_trailing_text() {
        let redactor = Arc::new(Redactor::new(vec!["random-secret-value".to_string()]));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("ordinary answer."), "ordinary answer.");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn streaming_redactor_redacts_an_unfinished_secret_prefix_on_flush() {
        let redactor = Arc::new(Redactor::new(vec!["random-secret-value".to_string()]));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("prefix random"), "prefix ");
        assert_eq!(stream.finish(), "[REDACTED]");
    }

    #[test]
    fn streaming_redactor_holds_bearer_suffixes_across_chunks() {
        let redactor = Arc::new(Redactor::new(Vec::new()));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("Authorization: Bear"), "");
        assert_eq!(
            stream.push("er abc.def done"),
            "Authorization: Bearer [REDACTED] done"
        );
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn streaming_redactor_holds_sk_tokens_across_chunks() {
        let redactor = Arc::new(Redactor::new(Vec::new()));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("value sk"), "value ");
        assert_eq!(stream.push("-proj-secret done"), "sk-[REDACTED] done");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn streaming_redactor_holds_assignment_patterns_across_chunks() {
        let redactor = Arc::new(Redactor::new(Vec::new()));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("token="), "");
        assert_eq!(stream.push("abc123 done"), "token=[REDACTED] done");
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn streaming_redactor_holds_url_credentials_across_chunks() {
        let redactor = Arc::new(Redactor::new(Vec::new()));
        let mut stream = StreamingRedactor::new(redactor);
        assert_eq!(stream.push("url https://alice"), "url ");
        assert_eq!(
            stream.push(":hunter2@example.test/path done"),
            "https://[REDACTED]example.test/path done"
        );
        assert!(stream.finish().is_empty());
    }

    #[test]
    fn masks_json_and_bearer_values() {
        let redactor = Redactor::new(Vec::new());
        let redacted =
            redactor.redact(r#"{"token":"abc","apiKey":"def"} Authorization: Bearer ghi.jkl"#);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("def"));
        assert!(!redacted.contains("ghi.jkl"));
    }
}
