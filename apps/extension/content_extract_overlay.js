/**
 * Click the extension action to outline extracted content blocks on the
 * current page. Click again to remove the overlay.
 */
(() => {
  const HOST_ID = "dw-extract-host";
  const existing = document.getElementById(HOST_ID);
  if (existing) {
    existing.remove();
    document.querySelectorAll("[data-dw-content-id]").forEach((el) => {
      el.removeAttribute("data-dw-content-id");
      el.style.removeProperty("outline");
      el.style.removeProperty("outline-offset");
      if (el.dataset.dwPrevDisplay !== undefined) {
        el.style.display = el.dataset.dwPrevDisplay;
        delete el.dataset.dwPrevDisplay;
      }
    });
    return;
  }

  if (typeof DwContentExtract === "undefined") {
    console.warn("DwContentExtract is not loaded");
    return;
  }

  const { blocks } = DwContentExtract.extractContent(document, {
    stamp: true,
    requireLayout: true,
  });
  const serialized = DwContentExtract.serializeBlocks(blocks);

  blocks.forEach((block) => {
    block.element.style.outline = "2px solid #6d28d9";
    block.element.style.outlineOffset = "2px";
  });

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
      .row { display: flex; gap: 6px; padding: 0 12px 10px; }
      button {
        font: inherit;
        border: 1px solid #e7e0d4;
        background: #fff;
        border-radius: 8px;
        padding: 4px 8px;
        cursor: pointer;
      }
      ol { margin: 0; padding: 0 12px 12px 28px; }
      li { margin: 0 0 8px; cursor: pointer; }
      li:hover { color: #6d28d9; }
      .kind { color: #6d28d9; font-weight: 650; }
    </style>
    <div class="panel">
      <header>컨텐츠 블록 ${serialized.length}개</header>
      <div class="meta">항목을 클릭하면 숨깁니다. 확장 아이콘을 다시 누르면 종료합니다.</div>
      <div class="row">
        <button id="copy" type="button">JSON 복사</button>
        <button id="reset" type="button">숨김 해제</button>
        <button id="close" type="button">닫기</button>
      </div>
      <ol>
        ${serialized
          .map(
            (b) =>
              `<li data-id="${b.id}"><span class="kind">${b.id} · ${b.kind}</span><br>${escapeHtml(
                (b.text || b.media.map((m) => m.kind + " " + (m.alt || "")).join(", ") || "(media)").slice(0, 160)
              )}</li>`
          )
          .join("")}
      </ol>
    </div>
  `;

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/"/g, "&quot;");
  }

  const byId = Object.fromEntries(blocks.map((b) => [b.id, b]));
  shadow.getElementById("copy").addEventListener("click", async () => {
    await navigator.clipboard.writeText(JSON.stringify(serialized, null, 2));
  });
  shadow.getElementById("reset").addEventListener("click", () => {
    blocks.forEach((b) => {
      if (b.element.dataset.dwPrevDisplay !== undefined) {
        b.element.style.display = b.element.dataset.dwPrevDisplay;
        delete b.element.dataset.dwPrevDisplay;
      }
    });
  });
  shadow.getElementById("close").addEventListener("click", () => {
    host.remove();
    blocks.forEach((b) => {
      b.element.style.removeProperty("outline");
      b.element.style.removeProperty("outline-offset");
      b.element.removeAttribute("data-dw-content-id");
      if (b.element.dataset.dwPrevDisplay !== undefined) {
        b.element.style.display = b.element.dataset.dwPrevDisplay;
        delete b.element.dataset.dwPrevDisplay;
      }
    });
  });
  shadow.querySelectorAll("li[data-id]").forEach((li) => {
    li.addEventListener("click", () => {
      const block = byId[li.getAttribute("data-id")];
      if (!block) {
        return;
      }
      if (block.element.dataset.dwPrevDisplay === undefined) {
        block.element.dataset.dwPrevDisplay = block.element.style.display || "";
        block.element.style.setProperty("display", "none", "important");
      }
      block.element.scrollIntoView({ block: "center" });
    });
  });

  console.table(serialized);
})();
