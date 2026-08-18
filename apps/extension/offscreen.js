setInterval(() => {
  chrome.runtime.sendMessage({ type: "heartbeat-tick" }).catch(() => {});
}, 3000);

chrome.runtime.sendMessage({ type: "heartbeat-tick" }).catch(() => {});
