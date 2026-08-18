use super::*;
use crate::http::{handle_request, origin_allowed, try_parse, HttpRequest};
use crate::rule::BrowserRule;
use persistence::MemoryPersistence;
use std::sync::Arc;
use word_based_filter::{WordBasedConfig, WordBasedFilterHandle, WordBasedStore};

fn memory_rule() -> BrowserRule {
    let store =
        BrowserStore::with_persistence(Arc::new(MemoryPersistence::<BrowserConfig>::new()))
            .unwrap();
    let rule = BrowserRule::new(store);
    let word_store = WordBasedStore::with_persistence(Arc::new(
        MemoryPersistence::<WordBasedConfig>::new(),
    ))
    .unwrap();
    let word = WordBasedFilterHandle::new(word_store);
    rule.register_content_filter(word.filter());
    rule
}

#[test]
fn origin_allows_extension_and_localhost() {
    assert!(origin_allowed(None));
    assert!(origin_allowed(Some(
        "chrome-extension://abcdefghijklmnopqrstuvwxyzabcdef"
    )));
    assert!(origin_allowed(Some("http://127.0.0.1:8766")));
    assert!(!origin_allowed(Some("https://evil.example")));
}

#[test]
fn parse_post_heartbeat() {
    let body = br#"{"browser":"x"}"#;
    let raw = format!(
        "POST /heartbeat HTTP/1.1\r\n\
Origin: chrome-extension://abc\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n",
        body.len()
    );
    let mut raw = raw.into_bytes();
    raw.extend_from_slice(body);
    let req = try_parse(&raw).unwrap().unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/heartbeat");
    assert_eq!(
        req.origin.as_deref(),
        Some("chrome-extension://abc")
    );
    assert_eq!(req.body, body);
}

#[test]
fn heartbeat_records_and_returns_blacklist() {
    let rule = memory_rule();
    rule.store().add_blacklist("youtube.com").unwrap();
    let req = HttpRequest {
        method: "POST".into(),
        path: "/heartbeat".into(),
        origin: Some("chrome-extension://abc".into()),
        body: br#"{"browser":"chrome","incognito_allowed":true,"has_blocking_permission":true}"#
            .to_vec(),
    };
    let resp = handle_request(&rule, &req);
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["blacklist"][0], "youtube.com");
    assert!(rule.heartbeats_snapshot().contains_key("chrome"));
}

#[test]
fn options_preflight_no_content() {
    let rule = memory_rule();
    let req = HttpRequest {
        method: "OPTIONS".into(),
        path: "/heartbeat".into(),
        origin: Some("chrome-extension://abc".into()),
        body: Vec::new(),
    };
    let resp = handle_request(&rule, &req);
    assert_eq!(resp.status, 204);
}

#[test]
fn unknown_path_is_404() {
    let rule = memory_rule();
    let req = HttpRequest {
        method: "GET".into(),
        path: "/nope".into(),
        origin: None,
        body: Vec::new(),
    };
    let resp = handle_request(&rule, &req);
    assert_eq!(resp.status, 404);
}

#[test]
fn evaluate_blocks_violent_item() {
    let rule = memory_rule();
    let body = serde_json::json!({
        "items": [
            { "id": "c1", "kind": "text", "text": "Nice flowers, thanks for sharing." },
            { "id": "c2", "kind": "text", "text": "I hate this and hope you suffer violently tonight." },
            {
                "id": "c3",
                "kind": "image",
                "text": "",
                "media": [{ "kind": "image", "alt": "nude still", "src": "https://cdn.example/a.jpg" }],
                "image": "data:image/jpeg;base64,aaaa"
            }
        ]
    });
    let req = HttpRequest {
        method: "POST".into(),
        path: "/evaluate".into(),
        origin: Some("chrome-extension://abc".into()),
        body: serde_json::to_vec(&body).unwrap(),
    };
    let resp = handle_request(&rule, &req);
    assert_eq!(resp.status, 200);
    let parsed: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["results"][0]["action"], "allow");
    assert_eq!(parsed["results"][1]["action"], "block");
    assert_eq!(parsed["results"][2]["action"], "block");
}
