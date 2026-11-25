use std::cmp::Reverse;

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
    fn masks_json_and_bearer_values() {
        let redactor = Redactor::new(Vec::new());
        let redacted =
            redactor.redact(r#"{"token":"abc","apiKey":"def"} Authorization: Bearer ghi.jkl"#);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("def"));
        assert!(!redacted.contains("ghi.jkl"));
    }
}
