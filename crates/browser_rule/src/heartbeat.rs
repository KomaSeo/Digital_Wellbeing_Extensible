use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Seconds a guarded browser may run after first appearance before a healthy
/// heartbeat is required. Avoids killing Chrome while the extension starts.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(30);

/// A heartbeat older than this is treated as missing (fail-closed).
pub const DEFAULT_HEARTBEAT_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatPayload {
    pub browser: String,
    pub incognito_allowed: bool,
    pub has_blocking_permission: bool,
}

#[derive(Debug, Clone)]
pub struct Heartbeat {
    pub browser: String,
    pub incognito_allowed: bool,
    pub has_blocking_permission: bool,
    pub received_at: Instant,
    pub received_at_utc: DateTime<Utc>,
}

impl Heartbeat {
    pub fn from_payload(payload: HeartbeatPayload) -> Self {
        Self {
            browser: payload.browser,
            incognito_allowed: payload.incognito_allowed,
            has_blocking_permission: payload.has_blocking_permission,
            received_at: Instant::now(),
            received_at_utc: Utc::now(),
        }
    }

    pub fn is_healthy(&self, now: Instant, ttl: Duration) -> bool {
        now.duration_since(self.received_at) <= ttl
            && self.incognito_allowed
            && self.has_blocking_permission
    }

    pub fn fail_reason(&self, now: Instant, ttl: Duration) -> Option<&'static str> {
        if now.duration_since(self.received_at) > ttl {
            Some("stale heartbeat")
        } else if !self.incognito_allowed {
            Some("incognito access disabled")
        } else if !self.has_blocking_permission {
            Some("missing blocking permission")
        } else {
            None
        }
    }
}

/// Last-seen heartbeats + grace window. The rule asks this type whether a
/// guarded browser is allowed to keep running.
pub struct HeartbeatMonitor {
    heartbeats: Mutex<HashMap<String, Heartbeat>>,
    first_seen: Mutex<HashMap<String, Instant>>,
    grace: Duration,
    ttl: Duration,
}

impl HeartbeatMonitor {
    pub fn new(grace: Duration, ttl: Duration) -> Self {
        Self {
            heartbeats: Mutex::new(HashMap::new()),
            first_seen: Mutex::new(HashMap::new()),
            grace,
            ttl,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn record(&self, mut payload: HeartbeatPayload) {
        payload.browser = payload.browser.trim().to_lowercase();
        tracing::debug!(
            browser = %payload.browser,
            incognito = payload.incognito_allowed,
            blocking = payload.has_blocking_permission,
            "browser heartbeat"
        );
        let hb = Heartbeat::from_payload(payload);
        self.heartbeats.lock().insert(hb.browser.clone(), hb);
    }

    pub fn snapshot(&self) -> HashMap<String, Heartbeat> {
        self.heartbeats.lock().clone()
    }

    /// Track which guarded browsers are running so grace starts on first sight
    /// and resets when the process disappears.
    pub fn observe_running(&self, browser_ids: &HashSet<String>, now: Instant) {
        let mut first_seen = self.first_seen.lock();
        first_seen.retain(|id, _| browser_ids.contains(id));
        for id in browser_ids {
            first_seen.entry(id.clone()).or_insert(now);
        }
    }

    /// `None` = keep running. `Some(reason)` = terminate.
    pub fn block_reason(&self, browser_id: &str, now: Instant) -> Option<String> {
        let heartbeats = self.heartbeats.lock();
        if let Some(hb) = heartbeats.get(browser_id) {
            if hb.is_healthy(now, self.ttl) {
                return None;
            }
        }
        let seen_at = self
            .first_seen
            .lock()
            .get(browser_id)
            .copied()
            .unwrap_or(now);
        if now.duration_since(seen_at) < self.grace {
            return None;
        }
        let reason = match heartbeats.get(browser_id) {
            None => "no heartbeat".to_string(),
            Some(hb) => hb
                .fail_reason(now, self.ttl)
                .unwrap_or("unhealthy heartbeat")
                .to_string(),
        };
        Some(reason)
    }

    #[cfg(test)]
    pub(crate) fn touch_first_seen(&self, browser_id: &str, at: Instant) {
        self.first_seen.lock().insert(browser_id.to_string(), at);
    }

    #[cfg(test)]
    pub(crate) fn age_heartbeat(&self, browser_id: &str, ago: Duration) {
        if let Some(hb) = self.heartbeats.lock().get_mut(browser_id) {
            hb.received_at = Instant::now().checked_sub(ago).unwrap_or_else(Instant::now);
        }
    }
}
