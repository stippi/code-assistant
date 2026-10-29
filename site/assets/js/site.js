// Behavior for the docs site: theme, frontend switch, tour, search, copy buttons.
(function () {
  const store = {
    get(key) { try { return localStorage.getItem(key); } catch (e) { return null; } },
    set(key, value) { try { localStorage.setItem(key, value); } catch (e) { /* private mode */ } },
  };

  // Theme: follows the system until the visitor picks one.
  const root = document.documentElement;
  document.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const dark = root.dataset.theme === "dark" ||
        (!root.dataset.theme && matchMedia("(prefers-color-scheme: dark)").matches);
      root.dataset.theme = dark ? "light" : "dark";
      store.set("ca-theme", root.dataset.theme);
    });
  });

  // Frontend switch: one choice for the whole site, remembered.
  const FRONTENDS = ["app", "tui", "acp"];
  function setFrontend(fe) {
    if (!FRONTENDS.includes(fe)) return;
    document.body.dataset.fe = fe;
    document.querySelectorAll("[data-fe-pick]").forEach((b) => {
      const on = b.dataset.fePick === fe;
      b.setAttribute(b.getAttribute("role") === "tab" ? "aria-selected" : "aria-pressed", String(on));
    });
    store.set("ca-fe", fe);
  }
  document.querySelectorAll("[data-fe-pick]").forEach((b) => {
    b.addEventListener("click", () => {
      // Keep the clicked group where it is while blocks above it change height.
      const top = b.getBoundingClientRect().top;
      setFrontend(b.dataset.fePick);
      window.scrollBy(0, b.getBoundingClientRect().top - top);
    });
  });
  const fromUrl = new URLSearchParams(location.search).get("fe");
  setFrontend(fromUrl || store.get("ca-fe") || document.body.dataset.fe || "app");

  // Tour: hotspots on the window drawing, explained in the side card.
  document.querySelectorAll("[data-tour]").forEach((tour) => {
    const steps = JSON.parse(tour.querySelector("script[type='application/json']").textContent);
    const hots = [...tour.querySelectorAll(".hot")];
    const el = (sel) => tour.querySelector(sel);
    let cur = 0;
    function show(i) {
      cur = (i + steps.length) % steps.length;
      el(".step").textContent = `${cur + 1} of ${steps.length}`;
      el("h3").textContent = steps[cur].title;
      el(".body").textContent = steps[cur].text;
      const link = el(".link");
      link.href = steps[cur].href;
      hots.forEach((h) => h.classList.toggle("on", Number(h.dataset.i) === cur));
    }
    hots.forEach((h) => h.addEventListener("click", () => show(Number(h.dataset.i))));
    el(".prev").addEventListener("click", () => show(cur - 1));
    el(".next").addEventListener("click", () => show(cur + 1));
    show(0);
  });

  // Copy buttons on code blocks.
  document.querySelectorAll(".prose .highlight, .split .highlight").forEach((block) => {
    const btn = document.createElement("button");
    btn.className = "copy-btn";
    btn.type = "button";
    btn.textContent = "Copy";
    btn.addEventListener("click", async () => {
      const code = block.querySelector("code");
      try {
        await navigator.clipboard.writeText(code ? code.innerText : "");
        btn.textContent = "Copied";
      } catch (e) {
        btn.textContent = "Press ⌘C";
      }
      setTimeout(() => (btn.textContent = "Copy"), 1400);
    });
    block.appendChild(btn);
  });

  // Mobile navigation.
  const menuBtn = document.querySelector("[data-menu-toggle]");
  if (menuBtn) menuBtn.addEventListener("click", () => document.body.classList.toggle("nav-open"));

  // Highlight the current heading in the table of contents.
  const tocLinks = [...document.querySelectorAll("aside.toc a")];
  if (tocLinks.length && "IntersectionObserver" in window) {
    const byId = new Map(tocLinks.map((a) => [decodeURIComponent(a.hash.slice(1)), a]));
    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        tocLinks.forEach((a) => a.classList.remove("on"));
        const link = byId.get(entry.target.id);
        if (link) link.classList.add("on");
      });
    }, { rootMargin: "-70px 0px -70% 0px" });
    byId.forEach((_, id) => { const h = document.getElementById(id); if (h) observer.observe(h); });
  }

  // Search over the JSON index Hugo writes next to the home page.
  const dialog = document.querySelector(".search-dialog");
  if (!dialog) return;
  const input = dialog.querySelector("input");
  const results = dialog.querySelector(".search-results");
  let index = null;
  let selected = 0;

  async function loadIndex() {
    if (index) return index;
    const res = await fetch(dialog.dataset.index);
    index = (await res.json()).map((p) => ({ ...p, hay: (p.title + " " + p.section + " " + p.text).toLowerCase() }));
    return index;
  }
  function score(page, terms) {
    let s = 0;
    for (const t of terms) {
      if (!page.hay.includes(t)) return 0;
      if (page.title.toLowerCase().includes(t)) s += 10;
      s += 1;
    }
    return s;
  }
  function snippet(text, term) {
    const i = text.toLowerCase().indexOf(term);
    if (i < 0) return text.slice(0, 120);
    const start = Math.max(0, i - 50);
    return (start ? "…" : "") + text.slice(start, start + 140) + "…";
  }
  function escape(s) {
    return s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  }
  async function render() {
    const q = input.value.trim().toLowerCase();
    if (!q) { results.innerHTML = ""; return; }
    const pages = await loadIndex();
    const terms = q.split(/\s+/);
    const hits = pages.map((p) => [score(p, terms), p]).filter(([s]) => s > 0).sort((a, b) => b[0] - a[0]).slice(0, 12);
    selected = 0;
    results.innerHTML = hits.length
      ? hits.map(([, p], i) => `<li><a href="${p.url}" class="${i === 0 ? "sel" : ""}">${escape(p.title)}<small>${escape(p.section)} · ${escape(snippet(p.text, terms[0]))}</small></a></li>`).join("")
      : `<li class="empty">Nothing found for “${escape(q)}”.</li>`;
  }
  function open() { dialog.classList.add("open"); input.value = ""; results.innerHTML = ""; input.focus(); loadIndex(); }
  function close() { dialog.classList.remove("open"); }
  document.querySelectorAll("[data-search-open]").forEach((b) => b.addEventListener("click", open));
  dialog.addEventListener("click", (e) => { if (e.target === dialog) close(); });
  input.addEventListener("input", render);
  input.addEventListener("keydown", (e) => {
    const links = [...results.querySelectorAll("a")];
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!links.length) return;
      selected = (selected + (e.key === "ArrowDown" ? 1 : -1) + links.length) % links.length;
      links.forEach((a, i) => a.classList.toggle("sel", i === selected));
      links[selected].scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter" && links[selected]) {
      location.href = links[selected].href;
    }
  });
  document.addEventListener("keydown", (e) => {
    const typing = /INPUT|TEXTAREA/.test(document.activeElement.tagName);
    if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) { e.preventDefault(); open(); }
    else if (e.key === "Escape") close();
  });
})();
