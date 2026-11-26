"use strict";
// The download page's list of the application's builds: the newest development release, named by its signed list (app/SHA256SUMS,
// staged with the site from that release). Display only: Peersus verifies the list's signature before it uses it, this page does
// not. One Download button opens the list of builds, the visitor's platform first; each file is fetched from the release it was
// published on, whose tag its signed name carries. Every value goes in through textContent or a checked attribute, never as markup.
// Run by app.js on every page it shows (the site changes pages without reloading): it does nothing where its elements are absent,
// and `signal` ends what it started on the document when the next page is shown.
const DOWNLOADS_ROOT = new URL(".", document.currentScript.src);
(window.peersusPages ||= []).push((signal) => {
  const actions = document.getElementById("get-actions");
  const list = document.getElementById("get-list");
  const status = document.getElementById("get-status");
  const version = document.getElementById("get-version");
  if (!actions || !list || !status) return;

  const RELEASES = "https://github.com/DarkPhilosophy/Peersus/releases/download/";
  // `peersus-<version>-<channel>.<build>-<platform>-<architecture>.<extension>`, as the application reads it (app_update.rs).
  const NAME = /^peersus-((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))-([a-z0-9]{1,16})\.([1-9][0-9]{0,19})-([a-z0-9_]{1,32})-([a-z0-9_]{1,32})(\.tar\.gz|\.AppImage|\.apk)$/;
  const PLATFORMS = { android: "Android", linux: "Linux", windows: "Windows", macos: "macOS" };
  const KINDS = { ".tar.gz": "archive", ".AppImage": "AppImage", ".apk": "package" };
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
  // The release's artifacts from its list: `<hex>  <name>` lines, a program's line (`<archive>/<program>`) passed over.
  function artifacts(body) {
    const found = [];
    for (const line of body.split("\n")) {
      const match = /^([0-9a-f]{64}) {2}([^/]+)$/.exec(line);
      const name = match && NAME.exec(match[2]);
      if (name) {
        found.push({ file: name[0], version: name[1], channel: name[2], build: name[3], platform: name[4], architecture: name[5], kind: KINDS[name[6]] });
      }
    }
    return found;
  }

  fetch(new URL("app/SHA256SUMS", DOWNLOADS_ROOT), { credentials: "omit", cache: "no-cache" })
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
      const found = artifacts(body);
      if (!found.length) throw new Error("Empty");
      const { version: release, channel, build } = found[0];
      if (found.some((artifact) => artifact.version !== release || artifact.channel !== channel || artifact.build !== build)) throw new Error("Invalid");
      const tag = `v${release}-${channel}.${build}`;
      const rank = (artifact) => (artifact.platform === yours ? -1 : Object.keys(PLATFORMS).indexOf(artifact.platform));
      found.sort((a, b) => rank(a) - rank(b));
      for (const artifact of found) {
        const row = el("li");
        const link = el("a");
        link.href = RELEASES + encodeURIComponent(tag) + "/" + encodeURIComponent(artifact.file);
        const name = PLATFORMS[artifact.platform] || artifact.platform;
        const platform = el("span", "store-menu-platform", name);
        if (artifact.platform === yours) platform.append(el("span", "store-menu-yours", " · this device"));
        const arch = ARCHITECTURES[artifact.architecture] ?? artifact.architecture;
        link.append(platform, el("span", "store-menu-size", [arch, artifact.kind].filter(Boolean).join(" · ")));
        row.append(link);
        list.append(row);
      }
      if (version) version.textContent = `v${release} · build ${build}`;
      actions.hidden = false;
      status.textContent = `${found.length} ${found.length === 1 ? "build" : "builds"}. Compare a download with its line in SHA256SUMS on the release; Peersus checks the signed list itself.`;
    })
    .catch(() => {
      status.textContent = "The prerelease list is unavailable right now.";
    });

  document.addEventListener("click", (event) => {
    const menu = actions.querySelector(".get-menu");
    if (menu && menu.open && !event.target.closest(".get-menu")) menu.open = false;
  }, { signal });
});
