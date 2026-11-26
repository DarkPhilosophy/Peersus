"use strict";
// No cookies, persistent storage, provider SDKs, or external requests.
//
// The site is one place: a link to another of its pages changes the page in place (its main part, header, footer, title and
// description) and puts its address in the history, without reloading. Back and Forward do the same. Only a link that leaves
// the site (GitHub, Ko-fi, an archive, `peersus://`) loads another document. Every page is still a whole page on its own, so a
// search engine, a reader without JavaScript or a page opened from a link gets the same words.
//
// Each script of the site gives `window.peersusPages` one function, run on every page shown, with an AbortSignal that ends
// what it started on the document when the next page is shown. A function finds its own elements and does nothing without them.
const SITE_ROOT = new URL(".", document.currentScript.src);
const calm = matchMedia("(prefers-reduced-motion: reduce)");
const pages = (window.peersusPages ||= []);

// Sections arrive as they are scrolled to. Nothing is hidden unless this runs (the rules are under `.js`), and nothing at all
// when motion is reduced or the browser has no IntersectionObserver: then everything is simply there.
pages.push((signal) => {
  const items = document.querySelectorAll(".reveal:not(.is-in)");
  if (!items.length || calm.matches || !("IntersectionObserver" in window)) return;
  document.documentElement.classList.add("js");
  const watcher = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      entry.target.classList.add("is-in");
      watcher.unobserve(entry.target);
    }
  }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
  items.forEach((item) => watcher.observe(item));
  signal.addEventListener("abort", () => watcher.disconnect());
});

// The theme button is in the header, which each page brings again; the chosen theme stays on the document for the visit.
pages.push((signal) => {
  const button = document.getElementById("theme");
  if (!button) return;
  button.hidden = false;
  const dark = () => {
    const explicit = document.documentElement.dataset.theme;
    return explicit ? explicit === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
  };
  const label = () => {
    const words = dark() ? "Light theme" : "Dark theme";
    button.textContent = words;
    button.setAttribute("aria-label", `Switch to ${words.toLowerCase()}`);
  };
  button.addEventListener("click", () => {
    document.documentElement.dataset.theme = dark() ? "light" : "dark";
    label();
  }, { signal });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", label, { signal });
  label();
});

// Supporters who chose to be thanked in public, on the support page.
pages.push(() => {
  const list = document.getElementById("supporters");
  const status = document.getElementById("supporter-status");
  if (!list || !status) return;
  fetch(new URL("supporters.json", SITE_ROOT), { credentials: "omit", cache: "no-cache" })
    .then((response) => { if (!response.ok) throw new Error("Unavailable"); return response.json(); })
    .then((data) => {
      if (!Array.isArray(data) || data.length > 200) throw new Error("Invalid public list");
      const fragment = document.createDocumentFragment();
      const names = new Set();
      for (const entry of data) {
        // Explicit consent is required even for a curated static list. Never interpret HTML.
        if (!entry || entry.consent !== true || typeof entry.displayName !== "string") continue;
        const name = entry.displayName.trim();
        if (!name || name.length > 80 || /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(name) || names.has(name)) continue;
        names.add(name);
        const item = document.createElement("li");
        item.textContent = name;
        fragment.append(item);
      }
      list.replaceChildren(fragment);
      status.textContent = names.size ? "Thank you for helping keep Peersus independent." : "No public acknowledgements yet.";
    })
    .catch(() => { status.textContent = "Public acknowledgements are unavailable right now. Thank you for your goodwill."; });
});

(function site() {
  let current = new AbortController();
  const run = () => {
    for (const page of pages) {
      try {
        page(current.signal);
      } catch (error) {
        console.error(error);
      }
    }
  };

  // A page of this site: same origin, under the site's folder, an HTML page or a folder, not a download, not for another window.
  function ours(link, event) {
    // A policy belongs to the document response. Do not adopt Architecture into
    // another page's stricter policy or keep its live-data policy on other pages.
    if (link.hasAttribute("data-document-nav") || document.getElementById("map-source")) return null;
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return null;
    if (link.target && link.target !== "_self") return null;
    if (link.hasAttribute("download") || link.getAttribute("rel")?.includes("external")) return null;
    const url = new URL(link.href, location.href);
    if (url.origin !== location.origin || !url.pathname.startsWith(SITE_ROOT.pathname)) return null;
    if (!/(\/|\.html)$/.test(url.pathname)) return null;
    return url;
  }

  let asked = 0;
  // The page shown: Back or Forward to another address of the same page only moves to a fragment, which the browser does.
  let shown = location.pathname + location.search;
  async function show(url, push) {
    const mine = ++asked;
    document.documentElement.classList.add("is-changing");
    let next;
    try {
      const response = await fetch(url, { credentials: "same-origin" });
      if (!response.ok) throw new Error("Unavailable");
      next = new DOMParser().parseFromString(await response.text(), "text/html");
    } catch {
      // The page could not be read here: let the browser load it, and say what it says.
      location.assign(url);
      return;
    }
    if (mine !== asked) return;
    const main = next.getElementById("main");
    const header = next.querySelector(".site-header");
    const footer = next.querySelector("body > footer");
    if (!main || !header || !footer) {
      location.assign(url);
      return;
    }
    current.abort();
    current = new AbortController();
    // The address first: what the new page's relative addresses lead to is decided by it.
    if (push) history.pushState(null, "", url);
    shown = url.pathname + url.search;
    const swap = () => {
      document.getElementById("main").replaceWith(document.adoptNode(main));
      document.querySelector(".site-header").replaceWith(document.adoptNode(header));
      document.querySelector("body > footer").replaceWith(document.adoptNode(footer));
      document.title = next.title;
      for (const selector of ['meta[name="description"]', 'link[rel="canonical"]', 'meta[property="og:title"]', 'meta[property="og:description"]', 'meta[property="og:url"]']) {
        const mine = document.head.querySelector(selector);
        const theirs = next.head.querySelector(selector);
        if (mine && theirs) mine.replaceWith(document.adoptNode(theirs));
      }
      for (const name of ["data-test-catalog"]) {
        if (next.body.hasAttribute(name)) document.body.setAttribute(name, next.body.getAttribute(name));
        else document.body.removeAttribute(name);
      }
    };
    if (document.startViewTransition && !calm.matches) await document.startViewTransition(swap).updateCallbackDone;
    else swap();
    document.documentElement.classList.remove("is-changing");
    const target = url.hash && document.getElementById(decodeURIComponent(url.hash.slice(1)));
    if (target) target.scrollIntoView();
    else if (push) window.scrollTo(0, 0);
    // A reader of the screen hears where they are, as after a page load.
    const heading = document.querySelector("#main h1");
    if (heading) {
      heading.setAttribute("tabindex", "-1");
      heading.focus({ preventScroll: true });
    }
    run();
  }

  if ("fetch" in window && history.pushState && window.DOMParser) {
    document.addEventListener("click", (event) => {
      const link = event.target.closest("a[href]");
      const url = link && ours(link, event);
      if (!url) return;
      // The page shown already: a fragment is the browser's to scroll to; the page itself goes back to its top.
      const same = (path) => path.replace(/\/index\.html$/, "/");
      if (same(url.pathname) === same(location.pathname) && url.search === location.search) {
        if (url.hash) return;
        event.preventDefault();
        window.scrollTo(0, 0);
        return;
      }
      event.preventDefault();
      show(url, true);
    });
    addEventListener("popstate", () => {
      if (location.pathname + location.search !== shown) show(new URL(location.href), false);
    });
  }
  run();
})();
