(function (root) {
  const FIXTURES = {
    community: {
      title: "커뮤니티 스레드",
      html: `
<header role="banner">
  <img alt="Logo" width="120" height="28" src="https://example.com/logo.png">
  <nav>
    <a href="/">Home</a>
    <a href="/explore">Explore</a>
  </nav>
</header>
<main>
  <article class="post">
    <h1>Welcome thread</h1>
    <p>This is the original post about gardening in the backyard.</p>
    <div class="actions">
      <button>Like</button>
      <button>Share</button>
    </div>
    <ul class="comments">
      <li class="comment">
        <p>Nice flowers, thanks for sharing with the group.</p>
        <button>Like</button>
        <button>Reply</button>
      </li>
      <li class="comment">
        <p>I hate this and hope you suffer violently tonight.</p>
        <button>Like</button>
      </li>
      <li class="comment">
        <p>Check the greenhouse tomorrow before noon.</p>
      </li>
    </ul>
  </article>
</main>
<aside>
  <h2>Rules</h2>
  <a href="/help">Help</a>
  <a href="/about">About</a>
  <a href="/terms">Terms</a>
</aside>
<footer>
  <a href="/terms">Terms</a> and <a href="/privacy">Privacy</a>
</footer>`,
    },
    video: {
      title: "영상 + 광고 + 댓글",
      html: `
<header role="banner">
  <img alt="Logo" class="logo" width="90" height="20" src="https://example.com/logo.png">
  <nav>Home Shorts Subscriptions</nav>
</header>
<main>
  <video src="https://example.com/watch.mp4" width="640" height="360"></video>
  <h1>Cute cats playing piano for one hour</h1>
  <iframe class="ad" src="https://ads.example/slot" width="300" height="250"></iframe>
  <section class="comments">
    <article class="comment"><p>The orange cat at 3:12 is hilarious and wholesome.</p></article>
    <article class="comment"><p>This video is packed with graphic violent scenes, skip it.</p></article>
  </section>
</main>
<footer role="contentinfo">Terms and Privacy</footer>`,
    },
    article: {
      title: "뉴스 본문",
      html: `
<nav><a href="/">Home</a><a href="/world">World</a></nav>
<article>
  <h1>City opens a new riverside park this weekend</h1>
  <p>Families gathered by the water for the opening ceremony at noon.</p>
  <p>Officials said the project took two years and cost very little extra.</p>
  <img alt="Photo of the riverside park" width="640" height="360" src="https://example.com/park.jpg">
</article>
<footer>Terms and Privacy</footer>`,
    },
  };

  function parseFixture(html) {
    const doc = document.implementation.createHTMLDocument("fixture");
    doc.body.innerHTML = html;
    return doc;
  }

  function texts(blocks) {
    return blocks.map((b) => b.text);
  }

  function joined(blocks) {
    return texts(blocks).join("\n");
  }

  async function runTests(api) {
    const { extractContent, serializeBlocks } = api;
    const results = [];

    async function test(name, fn) {
      try {
        await fn();
        results.push({ name, ok: true });
      } catch (err) {
        results.push({ name, ok: false, error: String(err && err.message ? err.message : err) });
      }
    }

    function assert(cond, message) {
      if (!cond) {
        throw new Error(message);
      }
    }

    await test("커뮤니티: 로고/약관/버튼은 빼고 본문·댓글만 남긴다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.community.html));
      const blob = joined(blocks).toLowerCase();
      assert(!blocks.some((b) => /\blogo\b/i.test(b.text)), "logo text leaked: " + joined(blocks));
      assert(!blob.includes("terms") && !blob.includes("privacy"), "legal chrome leaked: " + blob);
      assert(!blocks.some((b) => /^(like|share|reply)$/i.test(b.text.trim())), "button chrome became a block");
      assert(
        blocks.some((b) => /original post about gardening/i.test(b.text)),
        "missing original post"
      );
      assert(
        blocks.filter((b) => /nice flowers|suffer violently|greenhouse tomorrow/i.test(b.text)).length === 3,
        "expected 3 comments, got: " + joined(blocks)
      );
    });

    await test("커뮤니티: 유해 댓글이 별도 블록이고 element가 그 댓글 노드다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.community.html));
      const hit = blocks.find((b) => /suffer violently/i.test(b.text));
      assert(hit, "toxic comment missing");
      assert(hit.element, "missing element handle");
      assert(hit.element.matches("li, .comment") || hit.element.closest("li, .comment"), "handle is not the comment subtree: " + hit.element.tagName);
      assert(hit.element.matches("li.comment, .comment") || hit.tag === "LI", "expected the comment container, got " + hit.tag);
      assert(!/nice flowers/i.test(hit.text), "toxic block swallowed a neighbor comment");
    });

    await test("영상 페이지: video/iframe은 남기고 헤더·푸터는 뺀다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.video.html));
      const blob = joined(blocks).toLowerCase();
      assert(!blob.includes("terms"), "footer leaked");
      assert(
        blocks.some((b) => b.kind === "video" || b.media.some((m) => m.kind === "video")),
        "missing video block: " + JSON.stringify(serializeBlocks(blocks))
      );
      assert(
        blocks.some((b) => b.kind === "iframe" || b.media.some((m) => m.kind === "iframe")),
        "missing iframe/ad block"
      );
      assert(
        blocks.some((b) => /cute cats playing piano/i.test(b.text)),
        "missing video title"
      );
      assert(
        blocks.filter((b) => /hilarious and wholesome|graphic violent scenes/i.test(b.text)).length === 2,
        "expected 2 comments"
      );
    });

    await test("뉴스: 본문 문단과 이미지는 남긴다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.article.html));
      const blob = joined(blocks);
      assert(/riverside park/i.test(blob), "missing article text");
      assert(/two years/i.test(blob), "missing second paragraph");
      assert(
        blocks.some((b) => b.media.some((m) => m.kind === "image")),
        "missing image"
      );
      assert(!/terms/i.test(blob), "footer leaked");
    });

    await test("숨긴 노드는 추출하지 않는다", () => {
      const doc = parseFixture(`
        <p>Visible paragraph about a local bakery opening.</p>
        <p hidden>Secret hidden slur that should not be extracted.</p>
        <p aria-hidden="true">Another hidden line about violence.</p>
      `);
      const { blocks } = extractContent(doc);
      const blob = joined(blocks);
      assert(/local bakery opening/i.test(blob), "visible text missing");
      assert(!/secret hidden slur/i.test(blob), "hidden text leaked");
      assert(!/another hidden line/i.test(blob), "aria-hidden text leaked");
    });

    await test("serializeBlocks는 element를 떼고 id를 유지한다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.community.html));
      const json = serializeBlocks(blocks);
      assert(json.length === blocks.length, "length mismatch");
      assert(json.every((b) => b.id && b.kind && !("element" in b)), "element leaked into JSON");
    });

    await test("이미 찍힌 블록은 건너뛰고 새로 붙은 댓글만 추출한다", () => {
      const doc = parseFixture(FIXTURES.community.html);
      const first = extractContent(doc, { stamp: true });
      doc.querySelector(".comments").insertAdjacentHTML(
        "beforeend",
        '<li class="comment"><p>A newly loaded comment about compost bins appearing after scroll.</p></li>'
      );
      const second = extractContent(doc, {
        stamp: true,
        skipStamped: true,
        idStart: first.blocks.length + 1,
      });
      assert(second.blocks.length === 1, "expected 1 new block, got " + second.blocks.length + ": " + joined(second.blocks));
      assert(/compost bins/i.test(second.blocks[0].text), "missing new comment");
      assert(second.blocks[0].id === "c" + (first.blocks.length + 1), "id did not continue");
      assert(
        first.blocks.every((b) => b.element.getAttribute("data-dw-content-id")),
        "old stamps were lost"
      );
    });

    await test("범위 표시는 찍힌 extract 블록에 외곽선을 켠다", () => {
      const { setShowRanges, isShowRanges } = api;
      const doc = parseFixture(FIXTURES.community.html);
      extractContent(doc, { stamp: true });
      assert(setShowRanges(true, { document: doc }) === true, "setShowRanges did not return true");
      assert(isShowRanges() === true, "isShowRanges false after enable");
      assert(
        doc.documentElement.getAttribute("data-dw-show-content-ranges") === "1",
        "missing data-dw-show-content-ranges"
      );
      assert(doc.documentElement.classList.contains("dw-show-content-ranges"), "missing class");
      assert(doc.getElementById("dw-range-style"), "missing outline stylesheet");
      setShowRanges(false, { document: doc });
      assert(isShowRanges() === false, "isShowRanges true after disable");
      assert(!doc.documentElement.hasAttribute("data-dw-show-content-ranges"), "attr lingered");
    });

    await test("MutationObserver가 추가된 댓글을 이어서 검사한다", async () => {
      const { observeContent } = api;
      assert(typeof observeContent === "function", "observeContent missing");
      const doc = parseFixture(FIXTURES.community.html);
      const seen = [];
      const session = observeContent(doc, {
        debounceMs: 0,
        onBlocks: (chunk) => seen.push(...chunk),
      });
      assert(seen.length >= 5, "initial extract missing: " + joined(seen));
      const before = seen.length;
      doc.querySelector(".comments").insertAdjacentHTML(
        "beforeend",
        '<li class="comment"><p>Observer should catch this extra comment about mulch layers.</p></li>'
      );
      await new Promise((resolve) => setTimeout(resolve, 50));
      session.disconnect();
      assert(
        seen.length === before + 1,
        "expected one more block, got " + (seen.length - before) + " extra: " + joined(seen.slice(before))
      );
      assert(/mulch layers/i.test(seen[seen.length - 1].text), "new text missing");
    });

    return results;
  }

  root.DwContentExtractTest = { FIXTURES, parseFixture, runTests };
})(typeof globalThis !== "undefined" ? globalThis : this);
