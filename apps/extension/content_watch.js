/**
 * Always-on watcher: extract content blocks, send text+images to the local
 * Digital Wellbeing app, and hide anything that comes back as `block`.
 */
(() => {
  if (typeof DwContentExtract === "undefined") {
    console.warn("DwContentExtract is not loaded");
    return;
  }

  const { observeContent, serializeBlocks, setShowRanges } = DwContentExtract;
  const RANGE_STORAGE_KEY = "dwShowContentRanges";

  function persistShowRanges(on) {
    try {
      chrome.storage?.local?.set({ [RANGE_STORAGE_KEY]: Boolean(on) });
    } catch (_) {
      /* ignore */
    }
  }

  function applyShowRanges(on) {
    const next = setShowRanges(on);
    if (window.__dwWatchSession) {
      window.__dwWatchSession.showRanges = next;
    }
    if (typeof window.__dwWatchSession?.onRanges === "function") {
      window.__dwWatchSession.onRanges(next);
    }
    return next;
  }

  function bindRangeApi(session) {
    session.setShowRanges = (on) => {
      applyShowRanges(on);
      persistShowRanges(on);
    };
    session.showRanges = Boolean(DwContentExtract.isShowRanges());
  }

  if (window.__dwWatchSession) {
    bindRangeApi(window.__dwWatchSession);
    return;
  }
  const blocks = [];
  const byId = Object.create(null);
  const verdicts = Object.create(null);

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
    stop() {
      watch.disconnect();
    },
  };
  bindRangeApi(window.__dwWatchSession);

  try {
    chrome.storage?.local?.get(RANGE_STORAGE_KEY, (data) => {
      if (data && data[RANGE_STORAGE_KEY]) {
        applyShowRanges(true);
      }
    });
    chrome.storage?.onChanged?.addListener((changes, area) => {
      if (area === "local" && changes[RANGE_STORAGE_KEY]) {
        applyShowRanges(Boolean(changes[RANGE_STORAGE_KEY].newValue));
      }
    });
  } catch (_) {
    /* ignore */
  }
})();
