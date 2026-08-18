/**
 * Debug panel for the always-on content watcher.
 * Click the extension action to show/hide. Does not stop filtering.
 */
(() => {
  const HOST_ID = "dw-extract-host";
  const existing = document.getElementById(HOST_ID);
  if (existing) {
    existing.remove();
    return;
  }

  const session = window.__dwWatchSession;
  if (!session) {
    console.warn("content watch is not running on this page");
    return;
  }

  const host = document.createElement("div");
  host.id = HOST_ID;
  host.style.all = "initial";
  document.documentElement.appendChild(host);
  const shadow = host.attachShadow({ mode: "open" });
  shadow.innerHTML = `
    <style>
      :host { all: initial; }
      .panel {
        position: fixed;
        top: 12px;
        right: 12px;
        z-index: 2147483647;
        width: 360px;
        max-height: calc(100vh - 24px);
        overflow: auto;
        background: #fffaf3;
        color: #1c1917;
        border: 1px solid #e7e0d4;
        border-radius: 12px;
        box-shadow: 0 12px 40px rgba(0,0,0,.2);
        font: 13px/1.4 Segoe UI, sans-serif;
      }
      header { padding: 10px 12px 6px; font-weight: 650; }
      .meta { padding: 0 12px 8px; color: #57534e; font-size: 12px; }
      .row { display: flex; gap: 8px; align-items: center; padding: 0 12px 10px; }
      .toggle {
        display: flex;
        align-items: center;
        gap: 8px;
        flex: 1;
        cursor: pointer;
        user-select: none;
      }
      .toggle input {
        position: absolute;
        opacity: 0;
        pointer-events: none;
      }
      .switch {
        width: 36px;
        height: 20px;
        border-radius: 999px;
        background: #d6d3d1;
        position: relative;
        flex: none;
        transition: background 0.15s ease;
      }
      .switch::after {
        content: "";
        position: absolute;
        top: 2px;
        left: 2px;
        width: 16px;
        height: 16px;
        border-radius: 50%;
        background: #fff;
        box-shadow: 0 1px 2px rgba(0,0,0,.25);
        transition: transform 0.15s ease;
      }
      .toggle input:checked + .switch {
        background: #7c3aed;
      }
      .toggle input:checked + .switch::after {
        transform: translateX(16px);
      }
      button {
        font: inherit;
        border: 1px solid #e7e0d4;
        background: #fff;
        border-radius: 8px;
        padding: 4px 8px;
        cursor: pointer;
      }
      ol { margin: 0; padding: 0 12px 12px 28px; }
      li { margin: 0 0 8px; }
      .kind { color: #6d28d9; font-weight: 650; }
      .block { color: #9f1239; }
      .allow { color: #166534; }
    </style>
    <div class="panel">
      <header id="title">컨텐츠 필터</header>
      <div class="meta">텍스트와 이미지를 로컬 앱에 보내 평가합니다. block이면 요소를 숨깁니다.</div>
      <div class="row">
        <label class="toggle">
          <span>범위 표시</span>
          <input id="ranges" type="checkbox">
          <span class="switch"></span>
        </label>
        <button id="close" type="button">닫기</button>
      </div>
      <ol id="list"></ol>
    </div>
  `;

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/"/g, "&quot;");
  }

  function syncRangeToggle() {
    const on = Boolean(
      window.DwContentExtract?.isShowRanges?.() || session.showRanges
    );
    session.showRanges = on;
    shadow.getElementById("ranges").checked = on;
  }

  function applyRanges(on) {
    if (typeof session.setShowRanges === "function") {
      session.setShowRanges(on);
      return;
    }
    if (typeof window.DwContentExtract?.setShowRanges === "function") {
      session.showRanges = window.DwContentExtract.setShowRanges(on);
    }
  }

  function render() {
    const list = shadow.getElementById("list");
    list.innerHTML = "";
    const blocked = session.blocks.filter((b) => session.verdicts[b.id]?.action === "block").length;
    shadow.getElementById("title").textContent =
      `컨텐츠 필터  ${session.blocks.length}개 중 ${blocked}개 숨김`;
    for (const block of session.blocks) {
      const verdict = session.verdicts[block.id];
      const action = verdict?.action || "…";
      const li = document.createElement("li");
      li.innerHTML =
        `<span class="kind">${block.id}</span> ` +
        `<span class="${action === "block" ? "block" : "allow"}">${action}</span><br>` +
        escapeHtml((block.text || "(media)").slice(0, 140));
      list.appendChild(li);
    }
  }

  session.onVerdicts = () => render();
  session.onRanges = () => syncRangeToggle();
  render();
  syncRangeToggle();
  shadow.querySelector(".toggle").addEventListener("click", (event) => {
    event.preventDefault();
    applyRanges(!session.showRanges);
    syncRangeToggle();
  });
  shadow.getElementById("close").addEventListener("click", () => {
    session.onVerdicts = null;
    session.onRanges = null;
    host.remove();
  });
})();
