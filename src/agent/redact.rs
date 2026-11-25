use std::cmp::Reverse;
use std::sync::Arc;

use regex::Regex;

#[derive(Debug)]
pub(crate) struct Redactor {
    regexes: Vec<Regex>,
    secrets: Vec<String>,
}

impl Redactor {
    pub(crate) fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|secret| secret.len() >= 4);
        secrets.sort_by_key(|secret| Reverse(secret.len()));
        secrets.dedup();

        let patterns = [
            r"(?i)([a-z][a-z0-9+.-]*://)([^/\s:@]+):([^/\s@]+)@",
            r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]+",
            r#"(?i)(["'](?:token|password|passwd|secret|api[_-]?key|authorization|credential)["']\s*:\s*)(?:"(?:\\.|[^"])*"|'(?:\\.|[^'])*'|[^,}\s]+)"#,
            r#"(?i)\b(token|password|passwd|secret|api[_-]?key|authorization|credential)\b(\s*[:=]\s*)[^\s,;]+"#,
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
        let hold = self.redactor.pending_secret_prefix_bytes(&self.pending);
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
        self.redactor.redact(&pending)
    }

    pub(crate) fn reset(&mut self) {
        self.pending.clear();
    }
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
    fn masks_json_and_bearer_values() {
        let redactor = Redactor::new(Vec::new());
        let redacted =
            redactor.redact(r#"{"token":"abc","apiKey":"def"} Authorization: Bearer ghi.jkl"#);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("def"));
        assert!(!redacted.contains("ghi.jkl"));
    }
}
