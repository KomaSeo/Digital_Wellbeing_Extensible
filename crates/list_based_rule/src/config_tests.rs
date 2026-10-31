//! Responsibility: Test ListConfig: name normalization, blacklist/whitelist decisions,
//! protected names.

use super::*;

#[test]
fn test_normalize_name() {
    assert_eq!(ListConfig::normalize_name("folder/steam"), "steam.exe");
    assert_eq!(ListConfig::normalize_name("Steam"), "steam.exe");
    assert_eq!(ListConfig::normalize_name("steam.exe"), "steam.exe");
}

#[test]
fn blacklist_blocks_listed() {
    let mut config = ListConfig::default();
    config.blacklist.insert("steam.exe".into());
    assert!(config.should_block("Steam.EXE"));
    assert!(!config.should_block("notepad.exe"));
    assert!(!config.should_block("explorer.exe"));
}

#[test]
fn whitelist_blocks_unknown() {
    let mut config = ListConfig::default();
    config.mode = ListMode::Whitelist;
    config.whitelist.insert("notepad.exe".into());
    assert!(!config.should_block("notepad.exe"));
    assert!(config.should_block("steam.exe"));
    assert!(!config.should_block("csrss.exe"));
}

#[test]
fn test_always_protected() {
    let list_of_protected = always_protected();
    let config = ListConfig::default();
    for protected in list_of_protected {
        assert!(!config.should_block(protected));
    }
}
