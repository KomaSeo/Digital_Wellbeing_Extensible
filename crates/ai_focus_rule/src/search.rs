//! Responsibility: Treat search sites specially: a block on a search results page hits the
//! search terms (a pattern), not the whole site.
//!
//! Other sites are blocked as a whole (see [`crate::monitor::tab_key`]). Blocking Google or
//! Naver whole would take away every search the work needs, so an off-task search blocks only
//! searches like it: the judge writes a pattern (a word or a regular expression) for the
//! off-task topic, and the monitor closes search pages whose query the pattern matches.
//! Search sites are recognized by their tab titles (`funny cats - Google 검색`).

use crate::monitor::strip_browser_noise;
use enforcer::normalize_process_name;
use enforcer::pattern::matches_part;

/// The tab-title endings of search results pages, with the query before them.
const SEARCH_TITLE_ENDINGS: [&str; 9] = [
    " - Google 검색",
    " - Google Search",
    " : 네이버 검색",
    " – Daum 검색",
    " - Daum 검색",
    " | Daum 검색",
    " - Bing",
    " at DuckDuckGo",
    " - 검색 - Bing",
];

/// The longest pattern kept.
const MAX_PATTERN_CHARS: usize = 120;

/// The query of a search results page, lower case and trimmed; `None` for other pages.
pub fn query_of(title: &str) -> Option<String> {
    let page = strip_browser_noise(title);
    SEARCH_TITLE_ENDINGS
        .iter()
        .filter_map(|ending| page.strip_suffix(ending))
        .max_by_key(|query| query.len())
        .map(|query| query.trim().to_lowercase())
        .filter(|query| !query.is_empty())
}

/// The block-list key prefix of search blocks in `process`: `chrome.exe|search: `.
pub fn key_prefix(process_name: &str) -> String {
    format!("{}|search: ", normalize_process_name(process_name))
}

/// The block-list key for `pattern` in `process`.
pub fn key(process_name: &str, pattern: &str) -> String {
    format!("{}{}", key_prefix(process_name), pattern)
}

/// The pattern to block `query` with: the judge's `proposed` one when it is usable (it
/// matches this query, but neither nothing nor a single letter, so it cannot block every
/// search), otherwise the query itself.
pub fn pattern_for(query: &str, proposed: &str) -> String {
    let proposed = proposed.trim();
    let usable = !proposed.is_empty()
        && proposed.chars().count() <= MAX_PATTERN_CHARS
        && matches_part(proposed, query)
        && !matches_part(proposed, "")
        && !matches_part(proposed, "x");
    if usable {
        proposed.to_string()
    } else {
        query.to_string()
    }
}

/// Does the blocked search `pattern` cover `query`?
pub fn blocks(pattern: &str, query: &str) -> bool {
    matches_part(pattern, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_pages_are_recognized_by_their_titles() {
        assert_eq!(
            query_of("Funny Cats - Google 검색 - Google Chrome").as_deref(),
            Some("funny cats")
        );
        assert_eq!(
            query_of("rust async : 네이버 검색 - Whale").as_deref(),
            Some("rust async")
        );
        assert_eq!(query_of("Funny cats - YouTube - Google Chrome"), None);
        assert_eq!(query_of("Google - Google Chrome"), None);
    }

    #[test]
    fn a_pattern_never_blocks_every_search() {
        assert_eq!(
            pattern_for("funny cats", "funny cats?|cat videos"),
            "funny cats?|cat videos"
        );
        // Too broad, empty, or not even matching this search: the query itself.
        assert_eq!(pattern_for("funny cats", ".*"), "funny cats");
        assert_eq!(pattern_for("funny cats", "."), "funny cats");
        assert_eq!(pattern_for("funny cats", ""), "funny cats");
        assert_eq!(pattern_for("funny cats", "dogs"), "funny cats");
        assert!(blocks("funny cats?|cat videos", "best cat videos 2026"));
        assert!(!blocks("funny cats?|cat videos", "rust async tutorial"));
    }
}
