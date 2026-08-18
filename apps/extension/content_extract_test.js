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

  function runTests(api) {
    const { extractContent, serializeBlocks } = api;
    const results = [];

    function test(name, fn) {
      try {
        fn();
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

    test("커뮤니티: 로고/약관/버튼은 빼고 본문·댓글만 남긴다", () => {
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

    test("커뮤니티: 유해 댓글이 별도 블록이고 element가 그 댓글 노드다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.community.html));
      const hit = blocks.find((b) => /suffer violently/i.test(b.text));
      assert(hit, "toxic comment missing");
      assert(hit.element, "missing element handle");
      assert(hit.element.matches("li, .comment") || hit.element.closest("li, .comment"), "handle is not the comment subtree: " + hit.element.tagName);
      assert(hit.element.matches("li.comment, .comment") || hit.tag === "LI", "expected the comment container, got " + hit.tag);
      assert(!/nice flowers/i.test(hit.text), "toxic block swallowed a neighbor comment");
    });

    test("영상 페이지: video/iframe은 남기고 헤더·푸터는 뺀다", () => {
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

    test("뉴스: 본문 문단과 이미지는 남긴다", () => {
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

    test("숨긴 노드는 추출하지 않는다", () => {
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

    test("serializeBlocks는 element를 떼고 id를 유지한다", () => {
      const { blocks } = extractContent(parseFixture(FIXTURES.community.html));
      const json = serializeBlocks(blocks);
      assert(json.length === blocks.length, "length mismatch");
      assert(json.every((b) => b.id && b.kind && !("element" in b)), "element leaked into JSON");
    });

    test("stamp 옵션이 원본 노드에 id를 찍는다", () => {
      const doc = parseFixture(FIXTURES.community.html);
      const { blocks } = extractContent(doc, { stamp: true });
      const toxic = blocks.find((b) => /suffer violently/i.test(b.text));
      assert(toxic && toxic.element.getAttribute("data-dw-content-id") === toxic.id, "stamp missing");
    });

    return results;
  }

  root.DwContentExtractTest = { FIXTURES, parseFixture, runTests };
})(typeof globalThis !== "undefined" ? globalThis : this);
