//! Responsibility: Know browsers: which programs are browsers, and what page, site, YouTube
//! channel or search a tab shows, read from its window title.
//!
//! Everything this program knows about browsers is here, so the AI focus check (which decides
//! what to do with a tab) does not parse titles itself. A browser extension that reports the
//! address instead of the title (the owner's TODO) would replace this crate's guesses; the
//! rest of the program asks the same questions.

pub mod search;
pub mod youtube;

use enforcer::normalize_process_name;

/// Browser programs judged as tabs (closed with Ctrl+W) instead of apps, as the settings'
/// default (`ai_focus.json`'s `browsers`), in `normalize_process_name` form.
pub const DEFAULT_BROWSER_PROCESSES: [&str; 12] = [
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "whale.exe",
    "brave.exe",
    "opera.exe",
    // Linux process names (normalized like every name here)
    "chromium.exe",
    "chromium-browser.exe",
    "firefox-bin.exe",
    "firefox-esr.exe",
    "microsoft-edge.exe",
    "vivaldi-bin.exe",
];

/// The names browsers put at the end of their window titles ("… - Google Chrome"), lower
/// case. Edge writes a zero-width space inside its name.
pub const TITLE_NAMES: [&str; 10] = [
    "google chrome",
    "chromium",
    "microsoft edge",
    "microsoft\u{200b} edge",
    "mozilla firefox",
    "firefox",
    "whale",
    "brave",
    "opera",
    "vivaldi",
];

/// What separates the page from the browser's name in a title.
const NAME_SEPARATORS: [&str; 3] = [" - ", " — ", " – "];

/// The longest site name kept.
const MAX_SITE_CHARS: usize = 60;

/// `title` without a trailing browser name whose last part *contains* a name of
/// [`TITLE_NAMES`] (`… — Mozilla Firefox Private Browsing` too). Used where a missed name
/// would match too much (always-blocked sites, the lockdown's allowed pages).
pub fn without_browser_name(title: &str) -> &str {
    for separator in NAME_SEPARATORS {
        if let Some((page, last)) = title.rsplit_once(separator) {
            let last = last.trim().to_lowercase();
            if TITLE_NAMES.iter().any(|name| last.contains(name)) {
                return page;
            }
        }
    }
    title
}

/// A browser title without the browser's name (when the last part *is* a name of
/// [`TITLE_NAMES`]), Edge's "and 3 more pages" and unread counters such as "(3) ", which
/// change constantly: `(3) Funny cats - YouTube - Google Chrome` -> `Funny cats - YouTube`.
/// Block keys are built from it, so it strips only what is surely the browser's.
pub fn strip_browser_noise(title: &str) -> &str {
    let mut trimmed = title.trim();
    for separator in NAME_SEPARATORS {
        if let Some((page, last)) = trimmed.rsplit_once(separator) {
            let last = last.trim().to_lowercase();
            if TITLE_NAMES.iter().any(|name| last == *name) {
                trimmed = page;
                break;
            }
        }
    }
    // Edge: "Title and 3 more pages"
    if let Some(index) = trimmed
        .rfind(" and ")
        .filter(|_| trimmed.ends_with(" more pages"))
    {
        trimmed = &trimmed[..index];
    }
    if let Some(rest) = trimmed.strip_prefix('(') {
        if let Some((count, after)) = rest.split_once(") ") {
            if count
                .chars()
                .all(|character| character.is_ascii_digit() || character == '+')
            {
                trimmed = after;
            }
        }
    }
    trimmed.trim()
}

/// Best guess at the site from a browser window title, lower case:
/// `(3) Funny cats - YouTube - Google Chrome` -> `youtube`. Without a
/// separator the whole title is used.
pub fn site_of(title: &str) -> String {
    let stripped = strip_browser_noise(title);
    let site = [" - ", " | ", " — ", " · "]
        .iter()
        .filter_map(|separator| {
            stripped
                .rfind(separator)
                .map(|index| &stripped[index + separator.len()..])
        })
        .min_by_key(|candidate| candidate.len())
        .unwrap_or(stripped);
    let site = site.trim();
    let shortened: String = if site.chars().count() <= MAX_SITE_CHARS {
        site.to_string()
    } else {
        site.chars().take(MAX_SITE_CHARS).chain(['…']).collect()
    };
    shortened.to_lowercase()
}

/// Block key of a browser tab: `process|site`. A block per exact title would let the user
/// dodge it by moving to the next video or post; a block per site does not. (YouTube is
/// blocked per channel instead: [`youtube`]; a search per its terms: [`search`].)
pub fn tab_key(process_name: &str, title: &str) -> String {
    format!(
        "{}|{}",
        normalize_process_name(process_name),
        site_of(title)
    )
}

/// Pages a browser may keep open while a broken AI connection locks every app: Claude and
/// Anthropic pages (to get or fix a key), an empty new tab (to type their address), and the
/// Google sign-in page (to log in to Claude). Judged by the tab title, like every website.
pub fn lockdown_allows_page(title: &str) -> bool {
    let page = without_browser_name(title).trim().to_lowercase();
    page.is_empty()
        || ["claude", "anthropic", "google accounts", "google 계정"]
            .iter()
            .any(|allowed| page.contains(allowed))
        || [
            "new tab",
            "새 탭",
            "새 탭 페이지",
            "about:blank",
            "untitled",
            "start page",
        ]
        .iter()
        .any(|empty| page == *empty || page.starts_with(&format!("{empty} - ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_of_strips_browser_noise() {
        assert_eq!(
            site_of("(3) Funny cats - YouTube - Google Chrome"),
            "youtube"
        );
        assert_eq!(
            site_of("Inbox (2) - me@x.com - Gmail - Google Chrome"),
            "gmail"
        );
        assert_eq!(site_of("Rust | docs.rs — Mozilla Firefox"), "docs.rs");
        assert_eq!(site_of("New Tab"), "new tab");
    }

    /// Until rc.79 the block keys knew fewer browser names than the always-blocked sites did,
    /// so a Chromium or Vivaldi tab got "chromium" or "vivaldi" as its site.
    #[test]
    fn every_listed_browser_name_is_stripped_from_block_keys() {
        assert_eq!(site_of("Funny cats - YouTube - Chromium"), "youtube");
        assert_eq!(site_of("Funny cats - YouTube - Vivaldi"), "youtube");
        assert_eq!(site_of("Rust | docs.rs – Mozilla Firefox"), "docs.rs");
        assert_eq!(
            site_of("Video - YouTube - Microsoft\u{200b} Edge"),
            "youtube"
        );
        // A page whose last part only mentions a browser keeps it in its key.
        assert_eq!(site_of("Tickets - Opera House"), "opera house");
        assert_eq!(
            without_browser_name("Gmail — Mozilla Firefox Private Browsing"),
            "Gmail"
        );
    }

    #[test]
    fn every_browser_family_has_a_process_and_a_title_name() {
        for family in [
            "chrom", "edge", "firefox", "whale", "brave", "opera", "vivaldi",
        ] {
            assert!(
                DEFAULT_BROWSER_PROCESSES
                    .iter()
                    .any(|process| process.contains(family)),
                "{family}"
            );
            assert!(
                TITLE_NAMES.iter().any(|name| name.contains(family)),
                "{family}"
            );
        }
    }

    #[test]
    fn the_lockdown_keeps_claude_and_empty_tabs() {
        assert!(lockdown_allows_page("Claude - Google Chrome"));
        assert!(lockdown_allows_page(
            "API keys - Claude Console - Microsoft\u{200b} Edge"
        ));
        assert!(lockdown_allows_page("Anthropic"));
        assert!(lockdown_allows_page("새 탭 - Chrome"));
        assert!(lockdown_allows_page("New Tab"));
        assert!(!lockdown_allows_page("YouTube - Google Chrome"));
        assert!(!lockdown_allows_page("Gmail - Mozilla Firefox"));
    }
}
