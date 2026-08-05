use super::*;

#[test]
fn set_lists_replace_entirely() {
    let dir = std::env::temp_dir().join(format!(
        "dw-policy-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let store = PolicyStore::open(dir.join("policy.json")).unwrap();

    store
        .set_blacklist(&["Steam".into(), "discord.EXE".into(), "explorer.exe".into()])
        .unwrap();
    let p = store.get();
    assert!(p.blacklist.contains("steam.exe"));
    assert!(p.blacklist.contains("discord.exe"));
    assert!(!p.blacklist.contains("explorer.exe"));

    store
        .set_whitelist(&["code.exe".into(), "notepad.exe".into()])
        .unwrap();
    let p = store.get();
    assert!(p.whitelist.contains("code.exe"));
    assert!(p.whitelist.contains("notepad.exe"));
    assert!(p.whitelist.contains("csrss.exe"));

    let _ = std::fs::remove_dir_all(&dir);
}
