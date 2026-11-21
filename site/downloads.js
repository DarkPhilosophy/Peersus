"use strict";
// The download page's list of the application's builds: the development prerelease named by its signed manifest (app/update.json, staged
// with the site from the project's `app/test` release). Display only: Peersus verifies that manifest's signature before it uses
// it, this page does not. One Download button opens the list of builds, the visitor's platform first; each file is fetched from
// the release it was published on. Every value goes in through textContent or a checked attribute, never as markup.
// Run by app.js on every page it shows (the site changes pages without reloading): it does nothing where its elements are absent,
// and `signal` ends what it started on the document when the next page is shown.
const DOWNLOADS_ROOT = new URL(".", document.currentScript.src);
(window.peersusPages ||= []).push((signal) => {
  const actions = document.getElementById("get-actions");
  const list = document.getElementById("get-list");
  const status = document.getElementById("get-status");
  const version = document.getElementById("get-version");
  if (!actions || !list || !status) return;

  const RELEASE = "https://github.com/DarkPhilosophy/Peersus/releases/download/app%2Ftest/";
  const WORD = /^[a-z0-9_]{1,32}$/;
  const FILE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
  const PLATFORMS = { android: "Android", linux: "Linux", windows: "Windows", macos: "macOS" };
  const KINDS = { archive: "archive", image: "AppImage", package: "package" };
  const ARCHITECTURES = { any: "", aarch64: "ARM64", x86_64: "x86-64", arm: "ARM", x86: "x86" };
  const yours = (() => {
    const agent = navigator.userAgent;
    if (/Android/i.test(agent)) return "android";
    if (/iPhone|iPad|iPod/i.test(agent)) return null;
    if (/Windows/i.test(agent)) return "windows";
    if (/Macintosh|Mac OS X/i.test(agent)) return "macos";
    if (/Linux|X11|CrOS/i.test(agent)) return "linux";
    return null;
  })();

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const size = (bytes) => (bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MiB` : `${Math.max(1, Math.round(bytes / 1024))} KiB`);

  function valid(artifact) {
    return artifact && typeof artifact === "object" && Object.keys(artifact).length === 6
      && WORD.test(artifact.platform || "") && WORD.test(artifact.architecture || "") && Object.hasOwn(KINDS, artifact.kind)
      && typeof artifact.file === "string" && FILE.test(artifact.file) && /^[0-9a-f]{64}$/.test(artifact.sha256 || "")
      && Number.isInteger(artifact.size) && artifact.size > 0;
  }

  fetch(new URL("app/update.json", DOWNLOADS_ROOT), { credentials: "omit", cache: "no-cache" })
    .then((response) => {
      if (response.status === 404) return null;
      if (!response.ok) throw new Error("Unavailable");
      return response.text();
    })
    .then((body) => {
      if (body === null) {
        status.textContent = "No prerelease was staged with this site. Build it from source with the guide.";
        return;
      }
      if (body.length > 65536) throw new Error("Too large");
      const manifest = JSON.parse(body);
      if (!manifest || !Array.isArray(manifest.artifacts) || typeof manifest.version !== "string" || !Number.isInteger(manifest.build)) throw new Error("Invalid");
      const artifacts = manifest.artifacts.filter(valid);
      if (!artifacts.length) throw new Error("Empty");
      const rank = (artifact) => (artifact.platform === yours ? -1 : Object.keys(PLATFORMS).indexOf(artifact.platform));
      artifacts.sort((a, b) => rank(a) - rank(b));
      for (const artifact of artifacts) {
        const row = el("li");
        const link = el("a");
        link.href = RELEASE + encodeURIComponent(artifact.file);
        const name = PLATFORMS[artifact.platform] || artifact.platform;
        const platform = el("span", "store-menu-platform", name);
        if (artifact.platform === yours) platform.append(el("span", "store-menu-yours", " · this device"));
        const arch = ARCHITECTURES[artifact.architecture] ?? artifact.architecture;
        link.append(platform, el("span", "store-menu-size", [arch, KINDS[artifact.kind], size(artifact.size)].filter(Boolean).join(" · ")));
        row.append(link);
        list.append(row);
      }
      if (version) version.textContent = `v${manifest.version} · build ${manifest.build}`;
      actions.hidden = false;
      status.textContent = `${artifacts.length} ${artifacts.length === 1 ? "build" : "builds"}. Compare a download with its SHA-256 in the release notes; Peersus checks the signed manifest itself.`;
    })
    .catch(() => {
      status.textContent = "The prerelease list is unavailable right now.";
    });

  document.addEventListener("click", (event) => {
    const menu = actions.querySelector(".get-menu");
    if (menu && menu.open && !event.target.closest(".get-menu")) menu.open = false;
  }, { signal });
});
