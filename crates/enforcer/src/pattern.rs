//! Responsibility: Match process names and window titles against the user's block patterns:
//! plain text, or a regular expression when the pattern uses regex characters.
//!
//! A pattern is a regular expression when it contains one of `* + ? ( ) [ ] { } | ^ $ \`;
//! a dot alone does not count, so `steam.exe` stays a plain name. Matching ignores case.
//! A pattern that looks like a regular expression but does not compile is matched as plain
//! text. Compiled expressions are kept, since the same patterns are checked every second.

use parking_lot::Mutex;
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::OnceLock;

const REGEX_CHARACTERS: &str = "*+?()[]{}|^$\\";

/// Whether `pattern` is meant as a regular expression.
pub fn is_regex(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|character| REGEX_CHARACTERS.contains(character))
}

/// `pattern` matches all of `text` (a process name such as `steam.exe`).
pub fn matches_whole(pattern: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    if is_regex(pattern) {
        if let Some(expression) = compiled(&format!("^(?:{pattern})$")) {
            return expression.is_match(&text);
        }
    }
    pattern.trim().to_lowercase() == text
}

/// `pattern` matches a part of `text` (a browser tab title).
pub fn matches_part(pattern: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    if is_regex(pattern) {
        if let Some(expression) = compiled(pattern) {
            return expression.is_match(&text);
        }
    }
    let needle = pattern.trim().to_lowercase();
    !needle.is_empty() && text.contains(&needle)
}

/// The case-insensitive expression for `source`, or `None` when it does not compile.
fn compiled(source: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock();
    // The user's lists are short; a cap keeps a stream of edits from growing it forever.
    if cache.len() > 1000 {
        cache.clear();
    }
    cache
        .entry(source.to_string())
        .or_insert_with(|| {
            RegexBuilder::new(source)
                .case_insensitive(true)
                .size_limit(1 << 20)
                .build()
                .ok()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_and_regular_expressions_match_as_written() {
        assert!(matches_whole("steam.exe", "Steam.exe"));
        assert!(!matches_whole("steam.exe", "steamwebhelper.exe"));
        assert!(matches_whole(".*game.*", "mygame.exe"));
        assert!(matches_whole("(lol|valorant).*", "valorant-win64.exe"));
        assert!(!matches_whole(".*game.*", "notepad.exe"));
        // Not a valid expression: matched as plain text.
        assert!(matches_whole("weird(", "weird("));
        assert!(!matches_whole("weird(", "weird.exe"));
    }

    #[test]
    fn titles_match_by_part() {
        assert!(matches_part("youtube", "Funny cats - YouTube"));
        assert!(matches_part("reddit|twitter", "Home / Twitter"));
        assert!(matches_part("^funny", "Funny cats - YouTube"));
        assert!(!matches_part("^cats", "Funny cats - YouTube"));
        assert!(!matches_part("  ", "anything"));
    }
}
