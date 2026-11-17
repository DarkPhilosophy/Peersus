"use strict";
// The store page: tiles drawn from the signed approvals staged with this site (store/index.json and store/<id>/approval.json),
// or from the public test catalog in a development preview (store-test/catalog.json). Display only. Nothing here is a trust
// decision: Peersus verifies the maintainer's signature, and the size and SHA-256 of every archive, before it installs anything.
// Every fetch is same-origin and bounded, and every value goes in through textContent or an attribute that was checked
// first, never as markup. The checks below are the ones site/build.py makes when it stages a document, so a document
// that the builder would have refused is not shown either.
// Run by app.js on every page it shows (the site changes pages without reloading): it does nothing where its elements are absent,
// and `signal` ends what it started on the document when the next page is shown.
const STORE_ROOT = new URL(".", document.currentScript.src);
(window.peersusPages ||= []).push((signal) => {
  const list = document.getElementById("store-list");
  const status = document.getElementById("store-status");
  if (!list || !status) return;
  const search = document.getElementById("store-search");
  const platformFilter = document.getElementById("store-platform");
  const sortOrder = document.getElementById("store-sort");
  const empty = document.getElementById("store-empty");

  const test = document.body.dataset.testCatalog === "true";
  const base = new URL(test ? "store-test/" : "store/", STORE_ROOT).href;
  const CONTROL = /[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u;
  const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
  const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
  const DIGEST = /^[0-9a-f]{64}$/;
  const LINK = /^https:\/\/[^\s/?#@:]+(\/[^\s?#]*)?$/;
  const MAX_ARCHIVE = 128 * 1024 * 1024;
  const PLATFORMS = { android: "Android", linux: "Linux", windows: "Windows", macos: "macOS" };
  const ORDER = Object.keys(PLATFORMS);
  const ARCHITECTURES = { any: "any architecture", aarch64: "aarch64", x86_64: "x86-64", arm: "arm", x86: "x86" };

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const same = (object, keys) => {
    const own = Object.keys(object);
    return own.length === keys.length && keys.every((key) => own.includes(key));
  };
  const text = (value, limit) => typeof value === "string" && value.length <= limit && !CONTROL.test(value);
  const whole = (value) => typeof value === "number" && Number.isInteger(value);

  // The platform of the visitor's own device, to put its download first; null when the browser does not say.
  const yours = (() => {
    const agent = navigator.userAgent;
    if (/Android/i.test(agent)) return "android";
    if (/iPhone|iPad|iPod/i.test(agent)) return null;
    if (/Windows/i.test(agent)) return "windows";
    if (/Macintosh|Mac OS X/i.test(agent)) return "macos";
    if (/Linux|X11|CrOS/i.test(agent)) return "linux";
    return null;
  })();

  if (test) {
    document.getElementById("store-eyebrow").textContent = "EXTENSIONS / DEVELOPMENT PREVIEW";
    document.getElementById("store-title").textContent = "Test extensions";
    document.getElementById("store-lede").textContent = "Public signed fixtures that exercise the development store. They are not production approvals.";
  }

  function size(bytes) {
    if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(1)} MiB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
    return `${bytes} B`;
  }

  // One archive of an entry, or null when it is not what the builder accepts.
  function artifact(raw, grouped) {
    if (!raw || typeof raw !== "object" || !same(raw, grouped ? ["archive", "architecture", "platform", "sha256", "size"] : ["archive", "sha256", "size"])) return null;
    if (!text(raw.archive, 128) || !NAME.test(raw.archive) || !DIGEST.test(raw.sha256 || "") || !whole(raw.size) || raw.size < 1 || raw.size > MAX_ARCHIVE) return null;
    if (grouped && (!Object.hasOwn(PLATFORMS, raw.platform) || !Object.hasOwn(ARCHITECTURES, raw.architecture))) return null;
    return raw;
  }

  // One entry, or null when it is not what the builder accepts.
  function entry(raw, schema) {
    if (!raw || typeof raw !== "object") return null;
    const grouped = Object.hasOwn(raw, "artifacts");
    const keys = ["author", "description", "link", "name", "version"].concat(grouped ? ["artifacts"] : ["archive", "sha256", "size"]);
    if (!same(raw, keys) || (grouped && schema !== 2)) return null;
    if (!text(raw.name, 128) || !NAME.test(raw.name) || !text(raw.version, 32) || !VERSION.test(raw.version)) return null;
    if (!text(raw.author, 80) || !raw.author.trim() || !text(raw.description, 1000)) return null;
    if (!text(raw.link, 2048) || !LINK.test(raw.link)) return null;
    const files = grouped ? raw.artifacts : [{ archive: raw.archive, sha256: raw.sha256, size: raw.size }];
    if (!Array.isArray(files) || files.length < 1 || files.length > 32) return null;
    const checked = files.map((file) => artifact(file, grouped));
    if (checked.includes(null)) return null;
    if (grouped && new Set(checked.map((file) => `${file.platform}/${file.architecture}`)).size !== checked.length) return null;
    // The visitor's platform first, then the fixed order of platforms.
    const rank = (file) => (file.platform === yours ? -1 : ORDER.indexOf(file.platform));
    if (grouped) checked.sort((a, b) => rank(a) - rank(b));
    return { name: raw.name, version: raw.version, author: raw.author, description: raw.description, link: raw.link, files: checked, grouped };
  }

  // A colour of its own for every extension, the same on every visit: the page has no pictures of extensions to show.
  function hue(name) {
    let value = 0;
    for (const character of name) value = (value * 31 + character.codePointAt(0)) >>> 0;
    return value % 360;
  }

  function closeMenus(except) {
    for (const menu of document.querySelectorAll(".store-menu[open]")) if (menu !== except) menu.open = false;
  }

  // The one Download button of an extension: a menu of its archives, the visitor's platform first.
  function downloads(item) {
    const menu = el("details", "store-menu");
    const summary = el("summary", "store-button store-button-quiet store-button-large", "Download");
    summary.setAttribute("aria-label", `Download ${item.name}: choose an archive`);
    const choices = el("ul", "store-menu-list");
    for (const file of item.files) {
      const choice = el("li");
      const link = el("a");
      link.href = item.href(file.archive);
      if (!item.released) link.download = file.archive;
      const where = item.grouped ? PLATFORMS[file.platform] : "All platforms";
      const platform = el("span", "store-menu-platform", where);
      if (item.grouped && file.platform === yours) platform.append(el("span", "store-menu-yours", " · this device"));
      const detail = item.grouped && file.architecture !== "any" ? ` · ${ARCHITECTURES[file.architecture]}` : "";
      link.append(platform, el("span", "store-menu-size", size(file.size) + detail));
      choice.append(link);
      choices.append(choice);
    }
    menu.append(summary, choices);
    menu.addEventListener("toggle", () => {
      if (menu.open) closeMenus(menu);
    });
    return menu;
  }

  // The coloured banner and the letter tile every extension is drawn with, in a tile and in its details.
  // The extension's own pictures when its source gave the store some (an icon, screenshots), else its letter and colour.
  function picture(src, className) {
    const image = el("img", className);
    image.src = src;
    image.alt = "";
    image.loading = "lazy";
    image.decoding = "async";
    return image;
  }
  function art(item) {
    const banner = el("div", "store-banner");
    banner.setAttribute("aria-hidden", "true");
    const shot = item.shots[0];
    if (shot) banner.append(picture(shot, "store-banner-shot"));
    else banner.append(el("span", "store-banner-word", item.name));
    const icon = el("span", "store-icon");
    icon.setAttribute("aria-hidden", "true");
    if (item.icon) icon.append(picture(item.icon, "store-icon-picture"));
    else icon.textContent = item.name.slice(0, 1);
    return { banner, icon };
  }

  const badge = () => el("span", test ? "store-badge store-badge-test" : "store-badge", test ? "Test fixture" : "Approved");

  function platformsOf(item) {
    return item.grouped ? [...new Set(item.files.map((file) => file.platform))] : [];
  }

  function stat(value, label, tone) {
    const card = el("div", `store-stat${tone ? ` store-stat-${tone}` : ""}`);
    card.append(el("strong", "", value), el("span", "", label));
    return card;
  }

  // Everything about one extension, over the page: what it is, Install (opens Peersus at its store entry) and Download.
  function details(item) {
    const sheet = el("div", "store-sheet");
    sheet.style.setProperty("--hue", String(hue(item.name)));
    const { banner, icon } = art(item);
    const close = el("button", "store-close", "×");
    close.type = "button";
    close.setAttribute("aria-label", "Close");
    close.addEventListener("click", () => detail.close());
    const head = el("div", "store-sheet-head");
    const title = el("div", "store-sheet-title");
    title.append(el("h2", "store-sheet-name", item.name), badge());
    title.querySelector("h2").id = "store-detail-name";
    head.append(icon, title);
    sheet.append(banner, close, head);

    sheet.append(el("h3", "store-label", "Description"), el("p", "store-sheet-about", item.description || "No description."));
    if (item.shots.length) {
      sheet.append(el("h3", "store-label", "Screenshots"));
      const gallery = el("ul", "store-gallery");
      gallery.setAttribute("aria-label", `Screenshots of ${item.name}`);
      item.shots.forEach((shot, index) => {
        const row = el("li");
        const open = el("a", "store-shot");
        open.href = shot;
        open.target = "_blank";
        open.rel = "noopener";
        open.setAttribute("aria-label", `Screenshot ${index + 1} of ${item.name}, full size`);
        open.append(picture(shot, "store-shot-picture"));
        row.append(open);
        gallery.append(row);
      });
      sheet.append(gallery);
    }
    const covered = platformsOf(item);
    const tags = el("ul", "store-tags");
    tags.setAttribute("aria-label", "Platforms");
    for (const platform of covered.length ? covered : [null]) {
      tags.append(el("li", platform && platform === yours ? "store-yours" : "", platform ? PLATFORMS[platform] : "All platforms"));
    }
    sheet.append(tags);

    const sizes = item.files.map((file) => file.size);
    const stats = el("div", "store-stats");
    stats.append(
      stat(`v${item.version}`, "Version", "accent"),
      stat(String(covered.length || 1), covered.length === 1 ? "Platform" : "Platforms", "calm"),
      stat(size(Math.min(...sizes)), "Download", "blue"),
      stat(test ? "Test" : "Signed", test ? "Fixture key" : "By the maintainer", "warn"),
    );
    sheet.append(stats);

    const actions = el("div", "store-sheet-actions");
    const install = el("a", "store-button store-button-large store-install", `Install ${item.name}`);
    install.href = `peersus://store/${encodeURIComponent(item.name)}`;
    actions.append(install, downloads(item));
    sheet.append(actions);
    const hint = el("p", "store-hint");
    const get = el("a", "", "Get Peersus");
    get.href = new URL("install.html", STORE_ROOT).href;
    hint.append("Install opens Peersus at this extension in its store; Peersus checks the signature and the archive, then asks you to accept its licence and notice. Not installed yet? ", get, ".");
    sheet.append(hint);

    const panels = el("div", "store-panels");
    const files = el("section", "store-panel");
    files.append(el("h3", "store-panel-title", "Archives"));
    const rows = el("ul", "store-files");
    for (const file of item.files) {
      const row = el("li");
      const where = item.grouped ? `${PLATFORMS[file.platform]} · ${ARCHITECTURES[file.architecture]}` : "All platforms";
      const top = el("p", "store-file-top");
      top.append(el("span", "store-file-name", where), el("span", "store-file-size", size(file.size)));
      const digest = el("p", "store-digest");
      digest.append(el("span", "store-digest-label", "SHA-256"), el("code", "", file.sha256));
      row.append(top, digest);
      rows.append(row);
    }
    files.append(rows);
    const about = el("section", "store-panel");
    about.append(el("h3", "store-panel-title", "Details"));
    const facts = el("dl", "store-facts");
    const fact = (term, value) => {
      facts.append(el("dt", "", term));
      const dd = el("dd");
      dd.append(value);
      facts.append(dd);
    };
    fact("Author", item.author);
    // The author's address is text, not a link: this page does not vouch for it.
    fact("Author’s page", el("code", "store-url", item.link));
    if (item.approval) {
      const proof = el("span");
      const json = el("a", "", "approval.json");
      json.href = item.approval("approval.json");
      const signature = el("a", "", "approval.sig");
      signature.href = item.approval("approval.sig");
      proof.append(json, " · ", signature);
      fact("Signed approval", proof);
    }
    if (!test) {
      const own = el("a", "", `store/${item.name}.html`);
      own.href = new URL(`store/${encodeURIComponent(item.name)}.html`, STORE_ROOT).href;
      fact("Its page", own);
    }
    fact("Checked by", "Peersus, before it installs: this page verifies nothing");
    about.append(facts);
    panels.append(files, about);
    sheet.append(panels);
    return sheet;
  }

  const detail = document.getElementById("store-detail");

  function open(item) {
    if (!detail || typeof detail.showModal !== "function") return;
    detail.replaceChildren(details(item));
    if (!detail.open) detail.showModal();
    if (location.hash !== `#${item.name}`) history.replaceState(null, "", `#${encodeURIComponent(item.name)}`);
  }

  function tile(item) {
    const card = el("li", "store-tile");
    card.style.setProperty("--hue", String(hue(item.name)));
    const { banner, icon } = art(item);
    const body = el("div", "store-body");
    const name = el("h2", "store-name");
    // The whole tile opens the details; the name is the button, stretched over the tile.
    const button = el("button", "store-open", item.name);
    button.type = "button";
    button.addEventListener("click", () => open(item));
    name.append(button);
    body.append(name);
    if (item.description) body.append(el("p", "store-about", item.description));
    const covered = platformsOf(item);
    const meta = el("p", "store-meta");
    meta.append(
      el("span", "", `v${item.version}`),
      el("span", "", covered.length > 1 ? `${covered.length} platforms` : covered.length ? PLATFORMS[covered[0]] : "All platforms"),
      el("span", "", size(Math.min(...item.files.map((file) => file.size)))),
    );
    body.append(meta);
    card.append(banner, badge(), icon, body);
    return card;
  }

  let items = [];
  let skipped = 0;
  let summary = "";

  function draw() {
    const query = (search && search.value || "").trim().toLowerCase();
    const platform = platformFilter ? platformFilter.value : "";
    const shown = items
      .filter((item) => !query || `${item.name} ${item.author} ${item.description}`.toLowerCase().includes(query))
      .filter((item) => !platform || !item.grouped || item.files.some((file) => file.platform === platform));
    if (sortOrder && sortOrder.value === "size") {
      const smallest = (item) => Math.min(...item.files.map((file) => file.size));
      shown.sort((a, b) => smallest(a) - smallest(b) || a.name.localeCompare(b.name));
    } else {
      shown.sort((a, b) => a.name.localeCompare(b.name));
    }
    list.replaceChildren(...shown.map(tile));
    if (empty) empty.hidden = !(items.length && !shown.length);
    status.textContent = items.length && shown.length !== items.length ? `${shown.length} of ${summary}` : summary;
  }

  function present(found, total, noun, none) {
    items = found;
    skipped = total - found.length;
    const count = found.length === 1 ? "1 extension" : `${found.length} extensions`;
    summary = found.length
      ? `${count} ${noun}${skipped ? ` · ${skipped} not shown (failed this page’s checks)` : ""}`
      : none;
    if (platformFilter && yours) platformFilter.querySelector(`option[value="${yours}"]`)?.append(" (this device)");
    draw();
    // A link to one extension (store.html#name) opens its details.
    let asked = "";
    try {
      asked = decodeURIComponent(location.hash.slice(1));
    } catch {
      asked = "";
    }
    const item = asked && items.find((candidate) => candidate.name === asked);
    if (item) open(item);
  }

  for (const control of [search, platformFilter, sortOrder]) if (control) control.addEventListener("input", draw);
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".store-menu")) closeMenus(null);
  }, { signal });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeMenus(null);
  }, { signal });
  if (detail) {
    // A press on the dimmed backdrop, outside the sheet, closes it.
    detail.addEventListener("click", (event) => {
      const box = detail.getBoundingClientRect();
      if (event.clientX < box.left || event.clientX > box.right || event.clientY < box.top || event.clientY > box.bottom) detail.close();
    });
    detail.addEventListener("close", () => {
      closeMenus(null);
      if (location.hash) history.replaceState(null, "", location.pathname + location.search);
    });
  }

  const bounded = (response, limit) => {
    if (!response.ok) throw new Error("Unavailable");
    return response.text().then((body) => {
      if (body.length > limit) throw new Error("Too large");
      return JSON.parse(body);
    });
  };

  // The published store: one signed approval per extension, each for one version, gathered when the site was staged
  // (store/index.json lists them). The archives stay where the approval was published, on the project's release of that
  // version, and are linked there.
  function approvals(index) {
    if (!index || typeof index !== "object" || !same(index, ["media", "plugins", "schema"]) || index.schema !== 2 || !Array.isArray(index.plugins)
        || index.plugins.length > 512 || !index.plugins.every((id) => typeof id === "string" && /^[a-z][a-z0-9_-]{0,63}$/.test(id))
        || !index.media || typeof index.media !== "object") throw new Error("Invalid index");
    const PICTURE = /^(icon|screenshot-[1-8])\.(png|webp)$/;
    const media = (id) => {
      const names = Array.isArray(index.media[id]) ? index.media[id].filter((name) => typeof name === "string" && PICTURE.test(name)) : [];
      const at = (name) => `${base}${id}/media/${name}`;
      return { icon: names.filter((name) => name.startsWith("icon.")).map(at)[0] || null, shots: names.filter((name) => name.startsWith("screenshot-")).map(at) };
    };
    return Promise.all(index.plugins.map((id) =>
      fetch(`${base}${id}/approval.json`, { credentials: "omit", cache: "no-cache" })
        .then((response) => bounded(response, 65536))
        .then((document) => {
          if (!document || typeof document !== "object" || !same(document, ["entries", "schema", "sequence"]) || document.schema !== 2
              || !Array.isArray(document.entries) || document.entries.length !== 1) return null;
          const item = entry(document.entries[0], 2);
          if (!item || item.name !== id) return null;
          const release = `https://github.com/DarkPhilosophy/Peersus/releases/download/extension/${id}/v${item.version}/`;
          return { ...item, ...media(id), released: true, href: (archive) => release + archive, approval: (name) => `${base}${id}/${name}` };
        })
        .catch(() => null)))
      .then((found) => present(found.filter(Boolean), found.length, "approved by the maintainer", "No extension is approved yet."));
  }

  if (!test) {
    fetch(`${base}index.json`, { credentials: "omit", cache: "no-cache" })
      .then((response) => {
        if (response.status === 404) {
          status.textContent = "The store is not published yet: this site was staged without the store’s approvals. Nothing on this page is a placeholder.";
          return null;
        }
        return bounded(response, 65536).then(approvals);
      })
      .catch(() => {
        status.textContent = "The store is unavailable right now. Nothing is listed until it loads.";
      });
    return;
  }

  // The development preview: the public test fixture, one signed catalog with its archives beside it.
  fetch(`${base}catalog.json`, { credentials: "omit", cache: "no-cache" })
    .then((response) => {
      if (response.status === 404) return null;
      if (!response.ok) throw new Error("Unavailable");
      return response.text();
    })
    .then((body) => {
      if (body === null) {
        status.textContent = "The test catalog was not staged with this preview, so there is nothing to list.";
        return;
      }
      if (body.length > 1048576) throw new Error("Too large");
      const data = JSON.parse(body);
      if (!data || typeof data !== "object" || !same(data, ["entries", "schema", "sequence"]) || ![1, 2].includes(data.schema)
          || !whole(data.sequence) || data.sequence < 0 || !Array.isArray(data.entries) || data.entries.length > 512) throw new Error("Invalid catalog");
      const names = new Set();
      const found = [];
      for (const raw of data.entries) {
        const item = entry(raw, data.schema);
        if (!item || names.has(item.name)) continue;
        names.add(item.name);
        found.push({ ...item, icon: null, shots: [], released: false, href: (archive) => base + archive, approval: null });
      }
      present(found, data.entries.length, `in the test catalog (sequence ${data.sequence})`, "The test catalog lists no extensions.");
    })
    .catch(() => {
      status.textContent = "The test catalog is unavailable right now.";
    });
});
