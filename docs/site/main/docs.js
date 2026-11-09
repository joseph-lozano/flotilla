// Flotilla docs: the Contents button, copy buttons, heading links, the version picker and
// page search. Everything is relative to this folder, so a copied version keeps working.
(() => {
  document.documentElement.classList.add("js");
  const page = location.pathname.split("/").pop() || "index.html";

  // Contents, on narrow screens.
  const sidebar = document.querySelector(".sidebar");
  const toggle = document.querySelector(".toc-toggle");
  toggle?.addEventListener("click", () => {
    const open = sidebar.classList.toggle("open");
    toggle.setAttribute("aria-expanded", String(open));
  });

  // Copy buttons on commands.
  const copyIcon = '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5V4A1.5 1.5 0 0 0 9 2.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5"/></svg>';
  document.querySelectorAll(".cmd").forEach(block => {
    const pre = block.querySelector("pre");
    if (!pre || !navigator.clipboard) return;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "copy";
    const label = block.classList.contains("prompt") ? "Copy prompt" : "Copy";
    button.innerHTML = copyIcon + `<span>${label}</span>`;
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(pre.innerText.replace(/\n$/, ""));
        button.lastChild.textContent = "Copied";
      } catch {
        button.lastChild.textContent = "Couldn't copy";
      }
      setTimeout(() => (button.lastChild.textContent = label), 1600);
    });
    (block.querySelector(".label") || block).appendChild(button);
  });

  // A link on each section heading.
  document.querySelectorAll(".doc h2[id], .doc h3[id]").forEach(h => {
    const a = document.createElement("a");
    a.className = "anchor";
    a.href = "#" + h.id;
    a.textContent = "#";
    a.setAttribute("aria-label", "Link to " + h.textContent);
    h.appendChild(a);
  });

  // Versions: ../versions.json lists them; this folder's name is this version.
  const select = document.getElementById("version");
  const note = document.querySelector(".version-note");
  const here = location.pathname.split("/").slice(-2, -1)[0] || select?.dataset.current;
  fetch("../versions.json", { cache: "no-cache" })
    .then(r => (r.ok ? r.json() : Promise.reject()))
    .then(({ default: preferred, versions }) => {
      if (!select || !Array.isArray(versions) || !versions.length) return;
      const current = versions.find(v => v.path === here) || versions.find(v => v.path === select.dataset.current);
      select.replaceChildren(...versions.map(v => new Option(v.label || v.name, v.path, false, v === current)));
      select.addEventListener("change", async () => {
        const target = "../" + select.value + "/";
        let ok = false;
        try { ok = (await fetch(target + page, { method: "HEAD" })).ok; } catch {}
        location.href = ok ? target + page : target;
      });
      const best = versions.find(v => v.path === preferred);
      if (!note || !current || !best || current === best) return;
      const link = `<a href="../${best.path}/${page}">Read the ${best.name} docs</a>`;
      note.innerHTML = current.path === "main"
        ? `You're reading the docs for <code>main</code>, which may be ahead of ${best.name}, the newest release. ${link}.`
        : `These docs are for ${current.name}. The newest release is ${best.name}. ${link}.`;
      note.hidden = false;
    })
    .catch(() => {});

  // Search: the pages in the sidebar, fetched the first time you search, split at their headings.
  const input = document.getElementById("docs-search");
  const list = document.getElementById("search-results");
  if (!input || !list) return;
  let index = null;
  // An element's text with a space between its parts, so a table's cells or a card's lines
  // don't run together.
  const words = el => {
    const parts = [];
    const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    while (walk.nextNode()) parts.push(walk.currentNode.nodeValue);
    return parts.join(" ").replace(/\s+/g, " ").replace(/ ([.,;:!?)])/g, "$1").replace(/\( /g, "(").trim();
  };
  const build = () => index ??= Promise.all(
    [...document.querySelectorAll(".sidebar .pages > li > a")].map(async a => {
      const href = a.getAttribute("href");
      try {
        const html = await (await fetch(href)).text();
        const doc = new DOMParser().parseFromString(html, "text/html");
        const main = doc.querySelector(".doc");
        if (!main) return [];
        const title = main.querySelector("h1")?.textContent.trim() || a.textContent;
        const out = [];
        let entry = { href, title, heading: "", text: "" };
        for (const el of main.children) {
          if (/^H[23]$/.test(el.tagName) && el.id) {
            out.push(entry);
            entry = { href: href + "#" + el.id, title, heading: el.textContent.trim(), text: "" };
          } else if (!el.matches("nav, .version-note, .cards")) {
            entry.text += " " + words(el);
          }
        }
        out.push(entry);
        return out;
      } catch { return []; }
    })
  ).then(parts => parts.flat());

  const fold = s => s.toLowerCase().normalize("NFD").replace(/[̀-ͯ]/g, "");
  const escape = s => s.replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const quote = w => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  // A word matches where a word starts: "to" isn't in "laptop".
  const at = w => new RegExp("(^|[^a-z0-9])" + quote(w), "g");
  const count = (text, w) => (text.match(at(w)) || []).length;
  const snippet = (text, words, phrase) => {
    const flat = fold(text);
    const found = [flat.indexOf(phrase), ...words.map(w => flat.search(at(w)))].filter(i => i >= 0);
    const from = found.length ? Math.min(...found) : 0;
    const start = Math.max(0, from - 40);
    let s = (start ? "…" : "") + text.slice(start, start + 140).trim() + "…";
    s = escape(s);
    for (const w of words) s = s.replace(new RegExp("(^|[^\\p{L}\\p{N}])(" + quote(w) + ")", "giu"), "$1<mark>$2</mark>");
    return s;
  };

  let active = -1;
  const show = async () => {
    const q = input.value.trim();
    if (!q) { list.replaceChildren(); return; }
    const phrase = fold(q).replace(/\s+/g, " ");
    const all0 = phrase.split(" ").filter(Boolean);
    // Two-letter words ("to", "a") only count when they're all there is.
    const words = all0.filter(w => w.length > 2).length ? all0.filter(w => w.length > 2) : all0;
    const all = await build();
    if (input.value.trim() !== q) return;
    const scored = all.map(e => {
      const t = fold(e.title), h = fold(e.heading), x = fold(e.text);
      let score = 0;
      for (const w of words) {
        const inT = count(t, w), inH = count(h, w), inX = count(x, w);
        if (!inT && !inH && !inX) return null;
        score += (inT ? 4 : 0) + (inH ? 6 : 0) + Math.min(inX, 4);
      }
      if (words.length > 1 || all0.length > 1) {
        if (h.includes(phrase)) score += 14;
        if (x.includes(phrase)) score += 6 + Math.min(x.split(phrase).length - 2, 3) * 2;
      }
      if (!e.heading) score -= 3;
      return { e, score };
    }).filter(Boolean).sort((a, b) => b.score - a.score).slice(0, 8);
    active = -1;
    if (!scored.length) {
      list.innerHTML = '<li class="empty">Nothing matches</li>';
      return;
    }
    list.innerHTML = scored.map(({ e }) =>
      `<li><a href="${e.href}"><span class="where">${escape(e.title)}</span>` +
      `${escape(e.heading || e.title)}<span class="what">${snippet(e.text, words, phrase)}</span></a></li>`).join("");
  };
  input.addEventListener("focus", build, { once: true });
  input.addEventListener("input", show);
  input.addEventListener("keydown", e => {
    const links = [...list.querySelectorAll("a")];
    if (e.key === "Escape") { input.value = ""; list.replaceChildren(); return; }
    if (!links.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      active = (active + (e.key === "ArrowDown" ? 1 : -1) + links.length) % links.length;
      links.forEach((l, i) => l.classList.toggle("active", i === active));
      links[active].scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      e.preventDefault();
      location.href = links[Math.max(0, active)].href;
    }
  });
})();
