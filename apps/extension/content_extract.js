/**
 * Split a page into hideable content blocks (posts, comments, media)
 * after dropping chrome: nav, footer, buttons, logos, legal links.
 *
 * Blocks keep an `element` handle so a later pass can hide/redact them.
 */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.DwContentExtract = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  const SKIP_TAGS = new Set([
    "SCRIPT",
    "STYLE",
    "NOSCRIPT",
    "TEMPLATE",
    "LINK",
    "META",
    "HEAD",
    "BUTTON",
    "INPUT",
    "SELECT",
    "TEXTAREA",
    "OPTION",
    "LABEL",
    "NAV",
    "FOOTER",
    "FORM",
    "SVG",
    "CANVAS",
    "HR",
  ]);

  const SKIP_ROLES = new Set([
    "navigation",
    "banner",
    "contentinfo",
    "search",
    "menu",
    "menubar",
    "toolbar",
    "tablist",
    "tab",
    "button",
    "switch",
    "dialog",
    "alertdialog",
  ]);

  const MEDIA_SELECTOR = "img, video, audio, iframe, picture source, picture";

  const STRIP_FROM_TEXT =
    "script, style, noscript, svg, button, input, select, textarea, nav, footer, form, [role='button'], [aria-hidden='true']";

  const CHROME_WORDS = new Set(
    [
      "like",
      "likes",
      "liked",
      "share",
      "shares",
      "shared",
      "reply",
      "replies",
      "comment",
      "comments",
      "subscribe",
      "subscribed",
      "follow",
      "following",
      "signin",
      "login",
      "log",
      "in",
      "sign",
      "register",
      "home",
      "explore",
      "search",
      "notifications",
      "messages",
      "settings",
      "menu",
      "more",
      "show",
      "hide",
      "edit",
      "delete",
      "report",
      "save",
      "saved",
      "copy",
      "link",
      "terms",
      "privacy",
      "cookie",
      "cookies",
      "policy",
      "help",
      "about",
      "and",
      "or",
      "the",
      "a",
      "to",
      "for",
      "on",
      "at",
      "with",
      "by",
      "post",
      "posted",
      "view",
      "views",
      "watch",
      "later",
      "join",
      "next",
      "previous",
      "skip",
      "ad",
      "ads",
      "advertisement",
      "logo",
      "좋아요",
      "공유",
      "답글",
      "댓글",
      "구독",
      "팔로우",
      "로그인",
      "가입",
      "홈",
      "탐색",
      "검색",
      "알림",
      "메시지",
      "설정",
      "더보기",
      "수정",
      "삭제",
      "신고",
      "저장",
      "약관",
      "개인정보",
      "처리방침",
      "도움말",
    ].map((w) => w.toLowerCase())
  );

  const DEFAULTS = {
    minTextLength: 4,
    minMediaPx: 80,
    maxTextPerBlock: 1500,
    maxBlocks: 80,
    requireLayout: false,
    stamp: false,
    stampAttr: "data-dw-content-id",
    skipStamped: false,
    idStart: 1,
  };

  function extractContent(root, options) {
    const opts = Object.assign({}, DEFAULTS, options || {});
    const start = root && root.body ? root.body : root;
    if (!start) {
      return { blocks: [] };
    }

    const blocks = [];
    walk(start, blocks, opts);

    const sliced = blocks.slice(0, opts.maxBlocks).map((block, i) => {
      const id = "c" + (opts.idStart + i);
      if (opts.stamp && block.element && block.element.setAttribute) {
        block.element.setAttribute(opts.stampAttr, id);
      }
      return Object.assign({ id }, block);
    });

    return { blocks: sliced };
  }

  /**
   * Keep extracting as the page grows (infinite scroll, lazy comments).
   * Already-stamped nodes are skipped so only new blocks are returned.
   */
  function observeContent(root, options) {
    const opts = Object.assign({}, DEFAULTS, {
      stamp: true,
      skipStamped: true,
      debounceMs: 200,
      onBlocks: null,
    }, options || {});

    const target = root && root.body ? root.body : root;
    let nextId = opts.idStart || 1;
    let timer = null;
    let stopped = false;
    const pending = new Set();

    function scan(scanRoot) {
      if (stopped || !scanRoot) {
        return [];
      }
      const { blocks } = extractContent(scanRoot, Object.assign({}, opts, {
        skipStamped: true,
        stamp: true,
        idStart: nextId,
        maxBlocks: Math.max(opts.maxBlocks, 200),
      }));
      if (blocks.length) {
        nextId += blocks.length;
        if (typeof opts.onBlocks === "function") {
          opts.onBlocks(blocks);
        }
      }
      return blocks;
    }

    const initial = scan(target);
    const Observer = (target.ownerDocument || target).defaultView?.MutationObserver ||
      (typeof MutationObserver !== "undefined" ? MutationObserver : null);

    if (!Observer) {
      return { initial, disconnect() {} };
    }

    const observer = new Observer((mutations) => {
      if (stopped) {
        return;
      }
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (node.nodeType !== 1 || node.id === "dw-extract-host" || node.id === "dw-range-style") {
            continue;
          }
          pending.add(node.parentElement || node);
        }
      }
      if (pending.size === 0) {
        return;
      }
      clearTimeout(timer);
      timer = setTimeout(() => {
        const batch = [...pending];
        pending.clear();
        for (const parent of batch) {
          if (parent.isConnected) {
            scan(parent);
          }
        }
      }, opts.debounceMs);
    });

    observer.observe(target, { childList: true, subtree: true });

    return {
      initial,
      disconnect() {
        stopped = true;
        clearTimeout(timer);
        pending.clear();
        observer.disconnect();
      },
    };
  }

  function serializeBlocks(blocks) {
    return (blocks || []).map((block) => ({
      id: block.id,
      kind: block.kind,
      text: block.text,
      media: block.media,
      tag: block.tag,
      role: block.role,
      truncated: block.truncated || false,
    }));
  }

  const RANGE_STYLE_ID = "dw-range-style";
  const RANGE_ATTR = "data-dw-show-content-ranges";
  const RANGE_CLASS = "dw-show-content-ranges";
  let rangeShown = false;
  let rangeGuard = null;

  function rangeCss(stampAttr) {
    return `
      html[${RANGE_ATTR}] [${stampAttr}] {
        outline: 2px solid #7c3aed !important;
        outline-offset: 2px !important;
      }
      html[${RANGE_ATTR}] [${stampAttr}][data-dw-verdict="allow"] {
        outline-color: #16a34a !important;
      }
      html[${RANGE_ATTR}] [${stampAttr}][data-dw-verdict="block"] {
        outline-color: #e11d48 !important;
      }
    `;
  }

  function ensureRangeStyle(doc, stampAttr) {
    let style = doc.getElementById(RANGE_STYLE_ID);
    if (!style) {
      style = doc.createElement("style");
      style.id = RANGE_STYLE_ID;
      (doc.head || doc.documentElement).appendChild(style);
    }
    style.textContent = rangeCss(stampAttr);
  }

  function startRangeGuard(doc, stampAttr) {
    if (rangeGuard) {
      rangeGuard.disconnect();
      rangeGuard = null;
    }
    const Observer = doc.defaultView?.MutationObserver ||
      (typeof MutationObserver !== "undefined" ? MutationObserver : null);
    if (!Observer) {
      return;
    }
    rangeGuard = new Observer(() => {
      if (!rangeShown) {
        return;
      }
      if (!doc.documentElement.hasAttribute(RANGE_ATTR)) {
        doc.documentElement.setAttribute(RANGE_ATTR, "1");
      }
      if (!doc.documentElement.classList.contains(RANGE_CLASS)) {
        doc.documentElement.classList.add(RANGE_CLASS);
      }
      if (!doc.getElementById(RANGE_STYLE_ID)) {
        ensureRangeStyle(doc, stampAttr);
      }
    });
    rangeGuard.observe(doc.documentElement, {
      attributes: true,
      attributeFilter: [RANGE_ATTR, "class"],
      childList: true,
    });
    if (doc.head) {
      rangeGuard.observe(doc.head, { childList: true });
    }
  }

  function stopRangeGuard() {
    rangeGuard?.disconnect();
    rangeGuard = null;
  }

  /**
   * Draw outlines around stamped extract blocks. Independent of filtering.
   */
  function setShowRanges(on, options) {
    const opts = options || {};
    const doc = opts.document || (typeof document !== "undefined" ? document : null);
    if (!doc || !doc.documentElement) {
      return rangeShown;
    }
    const stampAttr = opts.stampAttr || DEFAULTS.stampAttr;
    rangeShown = Boolean(on);
    if (rangeShown) {
      doc.documentElement.setAttribute(RANGE_ATTR, "1");
      doc.documentElement.classList.add(RANGE_CLASS);
      ensureRangeStyle(doc, stampAttr);
      startRangeGuard(doc, stampAttr);
    } else {
      stopRangeGuard();
      doc.documentElement.removeAttribute(RANGE_ATTR);
      doc.documentElement.classList.remove(RANGE_CLASS);
      doc.getElementById(RANGE_STYLE_ID)?.remove();
    }
    return rangeShown;
  }

  function isShowRanges() {
    return rangeShown;
  }

  function walk(el, blocks, opts) {
    if (!el || el.nodeType !== 1) {
      // element가 존재하지않거나, 단순 노드인 경우 content가 없다고 판단함.
      return;
    }
    if (isStamped(el, opts)) {
      return;
    }
    if (shouldSkipSubtree(el, opts)) {
      return;
    }

    const groups = repeatingGroups(el, opts);
    if (groups.length > 0) {
      const grouped = new Set(groups.flat());
      for (const child of childElements(el)) {
        if (shouldSkipSubtree(child, opts)) {
          continue;
        }
        if (grouped.has(child)) {
          if (repeatingGroups(child, opts).length > 0) {
            walk(child, blocks, opts);
          } else {
            pushBlock(child, blocks, opts);
          }
        } else {
          walk(child, blocks, opts);
        }
      }
      return;
    }

    if (isMeaningfulMedia(el, opts) && !hasElementChildren(el)) {
      pushBlock(el, blocks, opts);
      return;
    }

    const kids = childElements(el).filter((child) => !shouldSkipSubtree(child, opts));
    if (kids.length === 0) {
      if (hasExtractableContent(el, opts) || isMeaningfulMedia(el, opts)) {
        pushBlock(el, blocks, opts);
      }
      return;
    }

    if (isMediaHost(el) && isMeaningfulMedia(el, opts) && !kids.some((k) => hasExtractableContent(k, opts))) {
      pushBlock(el, blocks, opts);
      return;
    }

    for (const child of kids) {
      walk(child, blocks, opts);
    }

    if (
      blocks.every((b) => b.element !== el && !el.contains(b.element)) &&
      hasDirectOwnText(el, opts) &&
      hasExtractableContent(el, opts)
    ) {
      pushBlock(el, blocks, opts);
    }
  }

  function isStamped(el, opts) {
    return Boolean(opts.skipStamped && el.getAttribute && el.getAttribute(opts.stampAttr));
  }

  function pushBlock(el, blocks, opts) {
    if (!el || isStamped(el, opts) || blocks.some((b) => b.element === el)) {
      return;
    }
    if (blocks.some((b) => b.element && (b.element.contains(el) || el.contains(b.element)))) {
      return;
    }
    if (!hasExtractableContent(el, opts) && !isMeaningfulMedia(el, opts)) {
      return;
    }

    const media = collectMedia(el, opts);
    let text = contentText(el);
    if (isChromeOnlyText(text)) {
      text = "";
    }
    if (!text && media.length === 0) {
      return;
    }

    let truncated = false;
    if (text.length > opts.maxTextPerBlock) {
      text = text.slice(0, opts.maxTextPerBlock).trim();
      truncated = true;
    }

    blocks.push({
      element: el,
      kind: classify(text, media),
      text,
      media,
      tag: el.tagName,
      role: el.getAttribute("role"),
      truncated,
    });
  }

  function classify(text, media) {
    const kinds = new Set(media.map((m) => m.kind));
    if (kinds.size === 0) {
      return "text";
    }
    if (!text && kinds.size === 1) {
      return [...kinds][0];
    }
    return "mixed";
  }

  function shouldSkipSubtree(el, opts) {
    if (el.nodeType !== 1) {
      return true;
    }
    if (el.id === "dw-extract-host") {
      return true;
    }
    const tag = el.tagName;
    if (SKIP_TAGS.has(tag)) {
      return true;
    }
    const role = (el.getAttribute("role") || "").toLowerCase();
    if (SKIP_ROLES.has(role)) {
      return true;
    }
    if (el.hidden || el.hasAttribute("hidden") || el.getAttribute("aria-hidden") === "true") {
      return true;
    }
    if (el.inert) {
      return true;
    }
    if (tag === "DIALOG" && !el.open) {
      return true;
    }
    if (
      tag === "HEADER" &&
      !el.querySelector("main, article, [role='main']") &&
      !el.closest("article, [role='article']")
    ) {
      return true;
    }
    if (isHiddenByLayout(el, opts)) {
      return true;
    }
    if (tag === "ASIDE" || role === "complementary") {
      const text = contentText(el);
      if (linkDensity(el, text) >= 0.5 || isChromeOnlyText(text)) {
        return true;
      }
    }
    return false;
  }

  function isHiddenByLayout(el, opts) {
    if (!opts.requireLayout) {
      return false;
    }
    const view = el.ownerDocument && el.ownerDocument.defaultView;
    if (!view || typeof view.getComputedStyle !== "function") {
      return false;
    }
    const style = view.getComputedStyle(el);
    if (!style) {
      return false;
    }
    if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") {
      return true;
    }
    const rect = el.getBoundingClientRect();
    return rect.width < 2 && rect.height < 2 && !isMeaningfulMedia(el, opts);
  }

  function childElements(el) {
    const out = [];
    if (el.children) {
      out.push(...el.children);
    }
    if (el.shadowRoot) {
      out.push(...el.shadowRoot.children);
    }
    return out;
  }

  function hasElementChildren(el) {
    return childElements(el).length > 0;
  }

  function repeatingGroups(el, opts) {
    const children = childElements(el).filter((child) => !shouldSkipSubtree(child, opts));
    if (children.length < 2) {
      return [];
    }
    const grouped = new Map();
    for (const child of children) {
      const sig = itemSignature(child);
      if (!grouped.has(sig)) {
        grouped.set(sig, []);
      }
      grouped.get(sig).push(child);
    }
    return [...grouped.values()].filter((group) => {
      if (group.length < 2) {
        return false;
      }
      const filled = group.filter(
        (item) => hasExtractableContent(item, opts) || isMeaningfulMedia(item, opts)
      );
      return filled.length >= 2;
    });
  }

  function itemSignature(el) {
    const classes = [...el.classList]
      .filter((c) => !/^(is-|has-|js-|css-|style-)/i.test(c))
      .slice(0, 4)
      .sort()
      .join(".");
    const childTags = [...el.children]
      .filter((child) => {
        const role = (child.getAttribute("role") || "").toLowerCase();
        return !SKIP_TAGS.has(child.tagName) && !SKIP_ROLES.has(role);
      })
      .slice(0, 8)
      .map((child) => child.tagName)
      .join(">");
    return el.tagName + "|" + classes + "|" + childTags;
  }

  function hasDirectOwnText(el, opts) {
    let len = 0;
    for (const node of el.childNodes) {
      if (node.nodeType === 3) {
        len += node.textContent.replace(/\s+/g, " ").trim().length;
      }
    }
    return len >= opts.minTextLength;
  }

  function hasExtractableContent(el, opts) {
    if (isMeaningfulMedia(el, opts)) {
      return true;
    }
    const text = contentText(el);
    if (!text || text.length < opts.minTextLength) {
      return false;
    }
    return !isChromeOnlyText(text);
  }

  function contentText(el) {
    const clone = el.cloneNode(true);
    clone.querySelectorAll(STRIP_FROM_TEXT).forEach((node) => node.remove());
    const raw = (clone.innerText || clone.textContent || "").replace(/\s+/g, " ").trim();
    return raw;
  }

  function isChromeOnlyText(text) {
    if (!text) {
      return true;
    }
    const stripped = text
      .replace(/[\d.,·•|/+\-_:;!?()[\]{}'"“”‘’]+/g, " ")
      .replace(/\s+/g, " ")
      .trim();
    if (!stripped) {
      return true;
    }
    const words = stripped.split(/\s+/);
    return words.every((w) => CHROME_WORDS.has(w.toLowerCase()));
  }

  function linkDensity(el, text) {
    const clone = el.cloneNode(true);
    clone.querySelectorAll(STRIP_FROM_TEXT).forEach((node) => node.remove());
    const all = (text || contentText(el)).length || 1;
    let linked = 0;
    clone.querySelectorAll("a").forEach((a) => {
      linked += (a.textContent || "").replace(/\s+/g, " ").trim().length;
    });
    return linked / all;
  }

  function isMediaHost(el) {
    return /^(IMG|VIDEO|AUDIO|IFRAME|PICTURE|SOURCE)$/.test(el.tagName);
  }

  function isMeaningfulMedia(el, opts) {
    if (el.matches && el.matches("video, audio, iframe")) {
      if (!opts.requireLayout) {
        return true;
      }
      return mediaBox(el).max >= opts.minMediaPx || Boolean(el.getAttribute("src"));
    }
    if (el.tagName === "PICTURE") {
      const img = el.querySelector("img");
      return img ? isMeaningfulMedia(img, opts) : false;
    }
    if (el.tagName === "IMG") {
      if (isLogoImage(el)) {
        return false;
      }
      const { width, height, max } = mediaBox(el);
      if (width === 1 && height === 1) {
        return false;
      }
      if (max >= opts.minMediaPx) {
        return true;
      }
      const alt = (el.getAttribute("alt") || "").trim();
      return !opts.requireLayout && alt.length >= 12;
    }
    return false;
  }

  function isLogoImage(el) {
    const blob = [
      el.getAttribute("alt") || "",
      el.getAttribute("aria-label") || "",
      el.id || "",
      (el.className || "").toString(),
    ]
      .join(" ")
      .toLowerCase();
    return /\blogo\b/.test(blob);
  }

  function mediaBox(el) {
    const attrW = parseInt(el.getAttribute("width") || "0", 10) || 0;
    const attrH = parseInt(el.getAttribute("height") || "0", 10) || 0;
    const width = el.naturalWidth || el.videoWidth || el.clientWidth || attrW;
    const height = el.naturalHeight || el.videoHeight || el.clientHeight || attrH;
    return { width, height, max: Math.max(width, height) };
  }

  function collectMedia(el, opts) {
    const nodes = [];
    if (isMediaHost(el)) {
      nodes.push(el);
    }
    el.querySelectorAll(MEDIA_SELECTOR).forEach((node) => {
      if (!nodes.includes(node) && !shouldSkipSubtree(node.parentElement || el, opts)) {
        nodes.push(node);
      }
    });

    const out = [];
    const seen = new Set();
    for (const node of nodes) {
      if (node.tagName === "SOURCE") {
        continue;
      }
      if (!isMeaningfulMedia(node, opts) && node.tagName !== "VIDEO" && node.tagName !== "IFRAME") {
        continue;
      }
      const desc = describeMedia(node);
      const key = desc.kind + "|" + desc.src + "|" + desc.alt;
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      out.push(desc);
    }
    return out;
  }

  function describeMedia(el) {
    const tag = el.tagName.toLowerCase();
    const kind = tag === "img" || tag === "picture" ? "image" : tag === "iframe" ? "iframe" : tag;
    const srcEl = tag === "picture" ? el.querySelector("img") || el : el;
    const rawSrc = srcEl.currentSrc || srcEl.src || srcEl.getAttribute("src") || "";
    return {
      kind,
      alt: (srcEl.getAttribute("alt") || srcEl.getAttribute("title") || "").trim(),
      src: shortenSrc(rawSrc),
    };
  }

  function shortenSrc(src) {
    if (!src || src.startsWith("data:")) {
      return src ? src.slice(0, 32) + "…" : "";
    }
    try {
      const url = new URL(src, "https://example.invalid");
      return url.host + url.pathname;
    } catch (_) {
      return src.slice(0, 120);
    }
  }

  return {
    extractContent,
    observeContent,
    serializeBlocks,
    setShowRanges,
    isShowRanges,
    DEFAULTS,
  };
});
