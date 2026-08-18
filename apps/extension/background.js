const HEARTBEAT_URL = "http://127.0.0.1:8766/heartbeat";
const EVALUATE_URL = "http://127.0.0.1:8766/evaluate";
const OFFSCREEN_URL = "offscreen.html";

ensureOffscreen();
chrome.runtime.onInstalled.addListener(() => {
  ensureOffscreen();
});
chrome.runtime.onStartup.addListener(() => {
  ensureOffscreen();
});

chrome.action.onClicked.addListener(async (tab) => {
  if (tab?.id == null) {
    return;
  }
  try {
    await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      files: ["content_extract.js", "content_watch.js", "content_extract_overlay.js"],
    });
  } catch (err) {
    console.warn("content extract inject failed", err);
  }
});

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.type === "evaluate-content") {
    evaluateContent(Array.isArray(msg.items) ? msg.items : [])
      .then((results) => sendResponse({ ok: true, results }))
      .catch((err) => sendResponse({ ok: false, error: String(err), results: [] }));
    return true;
  }
  if (msg?.type !== "heartbeat-tick") {
    return;
  }
  tick()
    .then(() => sendResponse({ ok: true }))
    .catch((err) => sendResponse({ ok: false, error: String(err) }));
  return true;
});

async function ensureOffscreen() {
  try {
    if (chrome.runtime.getContexts) {
      const existing = await chrome.runtime.getContexts({
        contextTypes: ["OFFSCREEN_DOCUMENT"],
        documentUrls: [chrome.runtime.getURL(OFFSCREEN_URL)],
      });
      if (existing.length > 0) {
        return;
      }
    }
    await chrome.offscreen.createDocument({
      url: OFFSCREEN_URL,
      reasons: ["DOM_PARSER"],
      justification:
        "Run a timer that heartbeats the local Digital Wellbeing app so the browser can be blocked if the extension is disabled.",
    });
  } catch (err) {
    const message = String(err);
    if (!message.includes("Only a single offscreen")) {
      console.warn("offscreen create failed", err);
    }
  }
}

async function tick() {
  const payload = await collectHeartbeat();
  const res = await fetch(HEARTBEAT_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    throw new Error(`heartbeat ${res.status}`);
  }
  const data = await res.json();
  await applyBlacklist(Array.isArray(data.blacklist) ? data.blacklist : []);
}

async function evaluateContent(items) {
  const res = await fetch(EVALUATE_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ items }),
  });
  if (!res.ok) {
    throw new Error(`evaluate ${res.status}`);
  }
  const data = await res.json();
  return Array.isArray(data.results) ? data.results : [];
}

async function collectHeartbeat() {
  const incognito_allowed = await chrome.extension.isAllowedIncognitoAccess();
  const has_blocking_permission = await chrome.permissions.contains({
    origins: ["<all_urls>"],
  });
  return {
    browser: await detectBrowser(),
    incognito_allowed,
    has_blocking_permission,
  };
}

async function detectBrowser() {
  const brands =
    navigator.userAgentData?.brands?.map((b) => b.brand.toLowerCase()) ?? [];
  if (brands.some((b) => b.includes("brave"))) {
    return "brave";
  }
  try {
    if (navigator.brave && (await navigator.brave.isBrave())) {
      return "brave";
    }
  } catch (_) {
    /* ignore */
  }
  const ua = navigator.userAgent || "";
  if (/Edg\//.test(ua)) {
    return "edge";
  }
  if (/Chrome\//.test(ua)) {
    return "chrome";
  }
  if (/Safari/i.test(ua)) {
    return "safari";
  }
  return "chrome";
}

async function applyBlacklist(hosts) {
  const existing = await chrome.declarativeNetRequest.getDynamicRules();
  const removeRuleIds = existing.map((r) => r.id);
  const addRules = hosts
    .map((host) => String(host).trim().toLowerCase())
    .filter(Boolean)
    .map((host, i) => ({
      id: i + 1,
      priority: 1,
      action: {
        type: "redirect",
        redirect: { extensionPath: "/blocked.html" },
      },
      condition: {
        urlFilter: `||${host}^`,
        resourceTypes: ["main_frame", "sub_frame"],
      },
    }));
  await chrome.declarativeNetRequest.updateDynamicRules({
    removeRuleIds,
    addRules,
  });
}
