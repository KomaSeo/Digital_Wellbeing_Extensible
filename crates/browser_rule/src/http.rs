use crate::heartbeat::HeartbeatPayload;
use crate::rule::BrowserRule;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8766;

const MAX_BODY: usize = 512_000;

#[derive(Debug)]
pub(crate) struct HttpRequest {
    pub method: String,
    pub path: String,
    pub origin: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct HttpResponse {
    pub status: u16,
    pub reason: &'static str,
    pub origin: Option<String>,
    pub body: Vec<u8>,
}

pub fn spawn_http(rule: Arc<BrowserRule>, port: u16) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = rx;
        if let Err(err) = serve(rule, port) {
            tracing::error!("browser heartbeat server failed: {err}");
        }
    });
    tx
}

fn serve(rule: Arc<BrowserRule>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    tracing::info!("browser heartbeat listening on 127.0.0.1:{port}");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let rule = Arc::clone(&rule);
                thread::spawn(move || {
                    if let Err(err) = handle_stream(&rule, &mut stream) {
                        tracing::debug!("heartbeat client error: {err}");
                    }
                });
            }
            Err(err) => {
                tracing::warn!("heartbeat accept error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

fn handle_stream(rule: &BrowserRule, stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = match read_request(stream) {
        Ok(req) => req,
        Err(err) => {
            let resp = json_response(400, "Bad Request", None, json!({ "ok": false, "error": err }));
            write_response(stream, &resp)?;
            return Ok(());
        }
    };
    let response = handle_request(rule, &request);
    write_response(stream, &response)
}

pub(crate) fn handle_request(rule: &BrowserRule, req: &HttpRequest) -> HttpResponse {
    if !origin_allowed(req.origin.as_deref()) {
        return json_response(
            403,
            "Forbidden",
            req.origin.clone(),
            json!({ "ok": false, "error": "origin not allowed" }),
        );
    }

    let path = req.path.split('?').next().unwrap_or(req.path.as_str());
    match (req.method.as_str(), path) {
        ("OPTIONS", _) => HttpResponse {
            status: 204,
            reason: "No Content",
            origin: req.origin.clone(),
            body: Vec::new(),
        },
        ("POST", "/heartbeat") => match parse_payload(&req.body) {
            Ok(payload) => {
                rule.record_heartbeat(payload);
                json_response(
                    200,
                    "OK",
                    req.origin.clone(),
                    json!({
                        "ok": true,
                        "blacklist": rule.store().blacklist_vec(),
                    }),
                )
            }
            Err(err) => json_response(
                400,
                "Bad Request",
                req.origin.clone(),
                json!({ "ok": false, "error": err }),
            ),
        },
        ("POST", "/evaluate") => match parse_evaluate(&req.body) {
            Ok(body) => {
                let results = rule.evaluate_content(&body.items);
                json_response(
                    200,
                    "OK",
                    req.origin.clone(),
                    json!({ "ok": true, "results": results }),
                )
            }
            Err(err) => json_response(
                400,
                "Bad Request",
                req.origin.clone(),
                json!({ "ok": false, "error": err }),
            ),
        },
        _ => json_response(
            404,
            "Not Found",
            req.origin.clone(),
            json!({ "ok": false, "error": "not found" }),
        ),
    }
}

fn parse_payload(body: &[u8]) -> Result<HeartbeatPayload, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid json: {e}"))?;
    serde_json::from_value(value).map_err(|e| format!("invalid heartbeat: {e}"))
}

fn parse_evaluate(body: &[u8]) -> Result<content_filter::EvaluateRequest, String> {
    serde_json::from_slice(body).map_err(|e| format!("invalid evaluate: {e}"))
}

pub(crate) fn origin_allowed(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) if o.starts_with("chrome-extension://") => true,
        Some(o) if o.starts_with("http://127.0.0.1") => true,
        Some(o) if o.starts_with("http://localhost") => true,
        _ => false,
    }
}

fn json_response(status: u16, reason: &'static str, origin: Option<String>, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        reason,
        origin,
        body: serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn write_response(stream: &mut TcpStream, resp: &HttpResponse) -> std::io::Result<()> {
    let origin = resp
        .origin
        .as_deref()
        .filter(|o| origin_allowed(Some(o)))
        .unwrap_or("*");
    let mut out = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: {}\r\n\
         Access-Control-Allow-Methods: POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         Access-Control-Allow-Private-Network: true\r\n\
         Access-Control-Max-Age: 600\r\n\
         \r\n",
        resp.status,
        resp.reason,
        resp.body.len(),
        origin
    )
    .into_bytes();
    out.extend_from_slice(&resp.body);
    stream.write_all(&out)?;
    stream.flush()
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(req) = try_parse(&buf)? {
            return Ok(req);
        }
        if buf.len() > MAX_BODY {
            return Err("request too large".into());
        }
    }
    try_parse(&buf)?.ok_or_else(|| "incomplete request".to_string())
}

pub(crate) fn try_parse(buf: &[u8]) -> Result<Option<HttpRequest>, String> {
    let header_end = match find_header_end(buf) {
        Some(i) => i,
        None => return Ok(None),
    };
    let header_bytes = &buf[..header_end];
    let rest = &buf[header_end..];
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.split('\n');
    let first = lines.next().unwrap_or("").trim_end_matches('\r');
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Err("missing request line".into());
    }

    let mut origin = None;
    let mut content_length = 0usize;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("Origin") {
            origin = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("Content-Length") {
            content_length = value.parse().unwrap_or(0);
        }
    }

    if content_length > MAX_BODY {
        return Err("body too large".into());
    }
    if rest.len() < content_length {
        return Ok(None);
    }
    let body = rest[..content_length].to_vec();
    Ok(Some(HttpRequest {
        method,
        path,
        origin,
        body,
    }))
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}
