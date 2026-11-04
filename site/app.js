"use strict";
// No cookies, persistent storage, provider SDKs, or external requests.
// Sections arrive as they are scrolled to. Nothing is hidden unless this script runs (the rules are under `.js`), and nothing at all
// when motion is reduced or the browser has no IntersectionObserver: then everything is simply there.
(function reveal() {
  const items = document.querySelectorAll(".reveal");
  const calm = matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (!items.length || calm || !("IntersectionObserver" in window)) return;
  document.documentElement.classList.add("js");
  const watcher = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      entry.target.classList.add("is-in");
      watcher.unobserve(entry.target);
    }
  }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
  items.forEach((item) => watcher.observe(item));
})();
const themeButton = document.getElementById("theme");
if (themeButton) {
  themeButton.hidden = false;
  function updateThemeLabel() {
    const explicit = document.documentElement.dataset.theme;
    const dark = explicit ? explicit === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
    const label = dark ? "Light theme" : "Dark theme";
    themeButton.textContent = label;
    themeButton.setAttribute("aria-label", `Switch to ${label.toLowerCase()}`);
  }
  themeButton.addEventListener("click", () => {
    const explicit = document.documentElement.dataset.theme;
    const dark = explicit ? explicit === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
    document.documentElement.dataset.theme = dark ? "light" : "dark";
    updateThemeLabel();
  });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", updateThemeLabel);
  updateThemeLabel();
}
const list = document.getElementById("supporters");
const status = document.getElementById("supporter-status");
if (list && status) {
  fetch("supporters.json", { credentials: "omit", cache: "no-cache" })
    .then(response => { if (!response.ok) throw new Error("Unavailable"); return response.json(); })
    .then(data => {
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
}

const extensionList = document.getElementById("extensions-list");
const extensionStatus = document.getElementById("extensions-status");
if (extensionList && extensionStatus) {
  const testCatalog = document.body.dataset.testCatalog === "true";
  if (testCatalog) {
    const heading = document.getElementById("extensions-title");
    if (heading) {
      heading.textContent = "Test extensions. Not production approvals.";
      const explanation = heading.nextElementSibling;
      if (explanation && explanation.tagName === "P") explanation.textContent = "These public signed fixtures exercise the development store. They do not configure or enable the production store.";
    }
  }
  // Display only. Nothing here is a trust decision: the application verifies the signed catalog itself. Same-origin
  // fetch, bounded, and every field goes in through textContent, never as markup.
  const text = (value, limit) => typeof value === "string" && value.length <= limit && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
  fetch(`${testCatalog ? "store-test" : "store"}/catalog.json`, { credentials: "omit", cache: "no-cache" })
    .then(response => { if (response.status === 404) return null; if (!response.ok) throw new Error("Unavailable"); return response.json(); })
    .then(data => {
      if (data === null) { extensionStatus.textContent = "No extensions have been published yet."; return; }
      if (!data || ![1, 2].includes(data.schema) || !Array.isArray(data.entries) || data.entries.length > 512) throw new Error("Invalid catalog");
      const fragment = document.createDocumentFragment();
      for (const entry of data.entries) {
        if (!entry || !text(entry.name, 128) || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(entry.name) || !text(entry.version, 32)
            || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(entry.version) || !text(entry.author, 80) || !entry.author.trim()
            || !text(entry.description, 1000) || !text(entry.link, 2048) || !/^https:\/\/[^\s/?#@:]+(\/[^\s?#]*)?$/.test(entry.link)) continue;
        const item = document.createElement("li");
        const title = document.createElement("h3");
        title.textContent = entry.name;
        const meta = document.createElement("p");
        meta.className = "extension-meta";
        meta.textContent = `${testCatalog ? "TEST FIXTURE · " : ""}by ${entry.author} · version ${entry.version}`;
        if (data.schema === 2 && Array.isArray(entry.artifacts)) {
          const platforms = [...new Set(entry.artifacts.map(artifact => artifact.platform))];
          if (platforms.some(platform => !["android", "linux", "macos", "windows"].includes(platform))) continue;
          meta.textContent += ` · ${platforms.map(platform => ({ android: "Android", linux: "Linux", macos: "macOS", windows: "Windows" })[platform]).join(", ")}`;
        }
        item.append(title, meta);
        if (entry.description) { const about = document.createElement("p"); about.textContent = entry.description; item.append(about); }
        const link = document.createElement("p");
        link.className = "extension-link";
        link.textContent = entry.link;
        item.append(link);
        fragment.append(item);
      }
      extensionList.replaceChildren(fragment);
      extensionStatus.textContent = extensionList.children.length
        ? (testCatalog ? "For isolated application testing, run ./store-demo.sh from the source checkout. Production trust is unchanged." : "Install them from Settings → Extensions → Store in Peersus.")
        : "No extensions have been published yet.";
    })
    .catch(() => { extensionStatus.textContent = testCatalog ? "The test catalog is unavailable right now." : "The list of approved extensions is unavailable right now."; });
}
