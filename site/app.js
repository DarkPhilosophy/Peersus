"use strict";
// No cookies, persistent storage, provider SDKs, or external requests.
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
  // Display only. Nothing here is a trust decision: the application verifies the signed catalog itself. Same-origin
  // fetch, bounded, and every field goes in through textContent, never as markup.
  const text = (value, limit) => typeof value === "string" && value.length <= limit && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
  fetch("store/catalog.json", { credentials: "omit", cache: "no-cache" })
    .then(response => { if (response.status === 404) return null; if (!response.ok) throw new Error("Unavailable"); return response.json(); })
    .then(data => {
      if (data === null) { extensionStatus.textContent = "No extensions have been published yet."; return; }
      if (!data || data.schema !== 1 || !Array.isArray(data.entries) || data.entries.length > 512) throw new Error("Invalid catalog");
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
        meta.textContent = `by ${entry.author} · version ${entry.version}`;
        item.append(title, meta);
        if (entry.description) { const about = document.createElement("p"); about.textContent = entry.description; item.append(about); }
        const link = document.createElement("p");
        link.className = "extension-link";
        link.textContent = entry.link;
        item.append(link);
        fragment.append(item);
      }
      extensionList.replaceChildren(fragment);
      extensionStatus.textContent = extensionList.children.length ? "Install them from Settings → Extensions → Store in Peersus." : "No extensions have been published yet.";
    })
    .catch(() => { extensionStatus.textContent = "The list of approved extensions is unavailable right now."; });
}
