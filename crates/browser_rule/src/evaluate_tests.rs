use super::*;
use crate::rule::BrowserRule;
use enforcer::{BlockRule, ProcessSnapshot};
use persistence::MemoryPersistence;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn memory_store() -> BrowserStore {
    BrowserStore::with_persistence(Arc::new(MemoryPersistence::<BrowserConfig>::new())).unwrap()
}

fn chrome() -> ProcessSnapshot {
    ProcessSnapshot {
        pid: 42,
        name: "chrome.exe".into(),
        exe: None,
    }
}

fn notepad() -> ProcessSnapshot {
    ProcessSnapshot {
        pid: 7,
        name: "notepad.exe".into(),
        exe: None,
    }
}

fn healthy_payload() -> HeartbeatPayload {
    HeartbeatPayload {
        browser: "chrome".into(),
        incognito_allowed: true,
        has_blocking_permission: true,
    }
}

#[test]
fn ignores_non_browser_processes() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::ZERO);
    let blocks = rule.evaluate(&[notepad()]);
    assert!(blocks.is_empty());
}

#[test]
fn grace_period_skips_block_without_heartbeat() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::from_secs(60), Duration::from_secs(10));
    let blocks = rule.evaluate(&[chrome()]);
    assert!(blocks.is_empty());
}

#[test]
fn blocks_after_grace_without_heartbeat() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::from_secs(1), Duration::from_secs(10));
    rule.touch_first_seen("chrome", Instant::now() - Duration::from_secs(30));
    let blocks = rule.evaluate(&[chrome()]);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].reason.contains("no heartbeat"));
}

#[test]
fn healthy_heartbeat_allows_browser() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::from_secs(10));
    rule.record_heartbeat(healthy_payload());
    let blocks = rule.evaluate(&[chrome()]);
    assert!(blocks.is_empty());
}

#[test]
fn missing_incognito_blocks() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::from_secs(10));
    rule.record_heartbeat(HeartbeatPayload {
        browser: "chrome".into(),
        incognito_allowed: false,
        has_blocking_permission: true,
    });
    let blocks = rule.evaluate(&[chrome()]);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].reason.contains("incognito"));
}

#[test]
fn missing_blocking_permission_blocks() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::from_secs(10));
    rule.record_heartbeat(HeartbeatPayload {
        browser: "chrome".into(),
        incognito_allowed: true,
        has_blocking_permission: false,
    });
    let blocks = rule.evaluate(&[chrome()]);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].reason.contains("blocking permission"));
}

#[test]
fn stale_heartbeat_blocks() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::from_secs(10));
    rule.record_heartbeat(healthy_payload());
    rule.age_heartbeat("chrome", Duration::from_secs(30));
    let blocks = rule.evaluate(&[chrome()]);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].reason.contains("stale"));
}

#[test]
fn brave_heartbeat_does_not_cover_chrome() {
    let rule = BrowserRule::with_timeouts(memory_store(), Duration::ZERO, Duration::from_secs(10));
    rule.record_heartbeat(HeartbeatPayload {
        browser: "brave".into(),
        incognito_allowed: true,
        has_blocking_permission: true,
    });
    let blocks = rule.evaluate(&[chrome()]);
    assert_eq!(blocks.len(), 1);
}
