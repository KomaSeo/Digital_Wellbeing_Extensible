//! Responsibility: Treat YouTube specially: name the channel (or, without one, the page) a
//! YouTube tab shows, so that a block on YouTube hits one channel instead of the whole site.
//!
//! Other sites are blocked as a whole (see [`crate::monitor::tab_key`]). YouTube serves
//! lectures and entertainment alike, so blocking it whole would also take the lectures away.
//! The window title only names the video, not its channel; the judge reads the channel name
//! from the screenshot and the monitor remembers it per page (see `MonitorState::channels`).

use crate::monitor::{site_of, strip_browser_noise};
use enforcer::normalize_process_name;

/// What [`site_of`] returns for a YouTube tab.
pub const SITE: &str = "youtube";

const MAX_NAME_CHARS: usize = 80;

/// Is this browser title a YouTube page? (`Video - YouTube - Google Chrome`.)
pub fn is_youtube(title: &str) -> bool {
    site_of(title) == SITE
}

/// The page part of a YouTube title, lower case and without the site, the browser and unread
/// counters: `(3) Funny cats - YouTube - Google Chrome` -> `funny cats`. The home page
/// (`YouTube`) gives `youtube`.
pub fn page_name(title: &str) -> String {
    let title_without_noise = strip_browser_noise(title);
    let page = title_without_noise
        .strip_suffix(" - YouTube")
        .or_else(|| title_without_noise.strip_suffix(" - youtube"))
        .unwrap_or(title_without_noise);
    clean(page)
}

/// A channel name as the judge read it, made comparable: trimmed, without a leading `@`,
/// inner whitespace collapsed, lower case. `None` for an empty name.
pub fn normalize_channel(name: &str) -> Option<String> {
    let name = clean(name.trim().trim_start_matches('@'));
    (!name.is_empty()).then_some(name)
}

/// Block key of a channel: `chrome.exe|youtube channel: some channel`.
pub fn channel_key(process_name: &str, channel: &str) -> String {
    format!(
        "{}|{SITE} channel: {channel}",
        normalize_process_name(process_name)
    )
}

/// Block key of a YouTube page whose channel is unknown (home, search results, or a video
/// judged without a screenshot): `chrome.exe|youtube page: funny cats`.
pub fn page_key(process_name: &str, title: &str) -> String {
    format!(
        "{}|{SITE} page: {}",
        normalize_process_name(process_name),
        page_name(title)
    )
}

fn clean(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_youtube_titles() {
        assert!(is_youtube("(3) Funny cats - YouTube - Google Chrome"));
        assert!(is_youtube("YouTube - Microsoft Edge"));
        assert!(!is_youtube("YouTube Music - Google Chrome"));
        assert!(!is_youtube("Rust | docs.rs — Mozilla Firefox"));
    }

    #[test]
    fn page_names_drop_noise() {
        assert_eq!(
            page_name("(3) Funny  cats - YouTube - Google Chrome"),
            "funny cats"
        );
        assert_eq!(page_name("YouTube - Google Chrome"), "youtube");
    }

    #[test]
    fn channel_names_are_comparable() {
        assert_eq!(
            normalize_channel("  @Rust  Lang "),
            Some("rust lang".to_string())
        );
        assert_eq!(normalize_channel(" "), None);
        assert_eq!(
            channel_key("Chrome", "rust lang"),
            "chrome.exe|youtube channel: rust lang"
        );
    }
}
