/**
 * Always-on watcher: extract content blocks, send text+images to the local
 * Digital Wellbeing app, and hide anything that comes back as `block`.
 */
(() => {
  if (window.__dwWatchSession) {
    return;
  }
  if (typeof DwContentExtract === "undefined") {
    console.warn("DwContentExtract is not loaded");
    return;
  }

  const { observeContent, serializeBlocks } = DwContentExtract;
  const RANGE_STYLE_ID = "dw-range-style";
  const RANGE_CLASS = "dw-show-content-ranges";
  const RANGE_STORAGE_KEY = "dwShowContentRanges";
  const blocks = [];
  const byId = Object.create(null);
  const verdicts = Object.create(null);
  let showRanges = false;

  function snapshotImage(block) {
    const node =
      block.element && block.element.tagName === "IMG"
        ? block.element
        : block.element && block.element.querySelector
          ? block.element.querySelector("img")
          : null;
    if (!node || !node.naturalWidth) {
      return null;
    }
    try {
      const max = 192;
      const scale = Math.min(1, max / Math.max(node.naturalWidth, node.naturalHeight));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(node.naturalWidth * scale));
      canvas.height = Math.max(1, Math.round(node.naturalHeight * scale));
      const ctx = canvas.getContext("2d");
      ctx.drawImage(node, 0, 0, canvas.width, canvas.height);
      const data = canvas.toDataURL("image/jpeg", 0.45);
      if (!data || data.length > 40_000) {
        return null;
      }
      return data;
    } catch (_) {
      return null;
    }
  }

  function toEvaluateItem(block) {
    const item = serializeBlocks([block])[0];
    const image = snapshotImage(block);
    if (image) {
      item.image = image;
    }
    delete item.tag;
    delete item.role;
    delete item.truncated;
    return item;
  }

  function hideBlock(block, reason) {
    if (!block.element || block.element.dataset.dwBlocked === "1") {
      return;
    }
    block.element.dataset.dwBlocked = "1";
    block.element.dataset.dwBlockReason = reason || "block";
    if (block.element.dataset.dwPrevDisplay === undefined) {
      block.element.dataset.dwPrevDisplay = block.element.style.display || "";
    }
    block.element.style.setProperty("display", "none", "important");
    block.element.setAttribute("aria-hidden", "true");
  }

  async function evaluateAndApply(newBlocks) {
    const items = newBlocks.map(toEvaluateItem);
    let results = [];
    try {
      const res = await chrome.runtime.sendMessage({
        type: "evaluate-content",
        items,
      });
      if (res && res.ok && Array.isArray(res.results)) {
        results = res.results;
      }
    } catch (err) {
      console.warn("content evaluate failed", err);
    }
    for (const result of results) {
      verdicts[result.id] = result;
      const block = byId[result.id];
      if (block?.element?.setAttribute) {
        block.element.setAttribute("data-dw-verdict", result.action || "");
      }
      if (result.action === "block") {
        if (block) {
          hideBlock(block, result.reason);
        }
      }
    }
    if (typeof window.__dwWatchSession.onVerdicts === "function") {
      window.__dwWatchSession.onVerdicts(results);
    }
  }

  function addBlocks(newBlocks) {
    for (const block of newBlocks) {
      blocks.push(block);
      byId[block.id] = block;
    }
    evaluateAndApply(newBlocks);
  }

  function ensureRangeStyle() {
    if (document.getElementById(RANGE_STYLE_ID)) {
      return;
    }
    const style = document.createElement("style");
    style.id = RANGE_STYLE_ID;
    style.textContent = `
      html.${RANGE_CLASS} [data-dw-content-id] {
        outline: 2px solid #7c3aed !important;
        outline-offset: 2px !important;
      }
      html.${RANGE_CLASS} [data-dw-content-id][data-dw-verdict="allow"] {
        outline-color: #16a34a !important;
      }
      html.${RANGE_CLASS} [data-dw-content-id][data-dw-verdict="block"] {
        outline-color: #e11d48 !important;
      }
    `;
    document.documentElement.appendChild(style);
  }

  function setShowRanges(on) {
    const next = Boolean(on);
    showRanges = next;
    if (window.__dwWatchSession) {
      window.__dwWatchSession.showRanges = next;
    }
    if (next) {
      ensureRangeStyle();
      document.documentElement.classList.add(RANGE_CLASS);
    } else {
      document.documentElement.classList.remove(RANGE_CLASS);
    }
    if (typeof window.__dwWatchSession?.onRanges === "function") {
      window.__dwWatchSession.onRanges(next);
    }
  }

  function persistShowRanges(on) {
    try {
      chrome.storage?.local?.set({ [RANGE_STORAGE_KEY]: Boolean(on) });
    } catch (_) {
      /* ignore */
    }
  }

  const watch = observeContent(document, {
    requireLayout: true,
    onBlocks(newBlocks) {
      addBlocks(newBlocks);
    },
  });

  window.__dwWatchSession = {
    blocks,
    byId,
    verdicts,
    showRanges: false,
    onVerdicts: null,
    onRanges: null,
    setShowRanges(on) {
      setShowRanges(on);
      persistShowRanges(on);
    },
    stop() {
      watch.disconnect();
    },
  };

  try {
    chrome.storage?.local?.get(RANGE_STORAGE_KEY, (data) => {
      if (data && data[RANGE_STORAGE_KEY]) {
        setShowRanges(true);
      }
    });
    chrome.storage?.onChanged?.addListener((changes, area) => {
      if (area === "local" && changes[RANGE_STORAGE_KEY]) {
        setShowRanges(Boolean(changes[RANGE_STORAGE_KEY].newValue));
      }
    });
  } catch (_) {
    /* ignore */
  }
})();
