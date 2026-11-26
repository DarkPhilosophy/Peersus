// Combined with the shared viewer at staging; not copied as a separate asset.
const publicationStatus = document.getElementById('map-source');
const isRecord = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const collection = (value, predicate, limit = 16384) => Array.isArray(value) && value.length <= limit && value.every(predicate);
const text = value => typeof value === 'string' && value.length <= 2048;
const identifier = value => typeof value === 'string' && /^[a-z0-9_.-]{1,300}$/i.test(value);
const kind = value => typeof value === 'string' && /^[a-z][a-z0-9-]{0,47}$/.test(value);
const natural = value => Number.isInteger(value) && value >= 0;
const optional = (value, check) => value === undefined || check(value);
const path = value => text(value) && /^[a-z0-9_./@+\[\]-]+$/i.test(value) &&
  value.split('/').every(part => part && part !== '.' && part !== '..');
const anchor = value => isRecord(value) && path(value.path) && natural(value.line) && value.line > 0;
const stringMap = value => isRecord(value) && Object.values(value).every(text);
const commitDate = value => typeof value === 'string' && /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/.test(value) &&
  Number.isFinite(Date.parse(value)) && new Date(value).toISOString() === value.replace(/Z$/, '.000Z');

function isPublication(data) {
  if (!isRecord(data) || data.schema !== PUBLIC_SCHEMA || !isRecord(data.source) ||
      typeof data.source.commit !== 'string' || !/^[0-9a-f]{40}$/.test(data.source.commit) || !commitDate(data.source.date) ||
      !isRecord(data.project) || !text(data.project.name) || !text(data.project.summary) ||
      !isRecord(data.peersus) || !isRecord(data.peersus.boards) || !isRecord(data.peersus.diagrams)) return false;
  const atom = v => v === null || text(v) || typeof v === 'boolean' || natural(v);
  const facts = v => isRecord(v) && Object.values(v).every(a => atom(a) || collection(a, atom, 128));
  const doc = v => v === null || isRecord(v) && path(v.path) && text(v.heading) && text(v.slug);
  if (!collection(data.nodes, n => isRecord(n) && identifier(n.id) && (n.parent === null || identifier(n.parent)) &&
      kind(n.kind) && text(n.label) && text(n.summary) && anchor(n.source) && optional(n.doc, doc) &&
      optional(n.inventoryId, text) && optional(n.details, text) && optional(n.evidenceKind, text) && optional(n.facts, facts), 4096)) return false;
  const nodes = new Map(data.nodes.map(n => ['n:' + n.id, n]));
  if (nodes.size !== data.nodes.length) return false;
  const ref = v => nodes.has(v);
  const container = v => v === 'root' || ref(v);
  for (const node of nodes.values()) {
    let at = node, depth = 0;
    while (at.parent !== null) {
      at = nodes.get('n:' + at.parent);
      if (!at || ++depth > 64) return false;
    }
  }
  const {boards, diagrams, views, tourAction, violations} = data.peersus;
  const paths = v => collection(v, ref, 128);
  if (!Object.hasOwn(boards, 'root') || !Object.hasOwn(diagrams, 'root') || !boards.root.cards?.length ||
      !Object.entries(boards).every(([key, b]) => container(key) && isRecord(b) && paths(b.cards) &&
        ['read', 'title', 'badge', 'brief', 'inspector'].every(k => optional(b[k], text)) &&
        ['adjacentLayers', 'showViolations'].every(k => optional(b[k], v => typeof v === 'boolean')) &&
        ['kinds', 'groupCaption'].every(k => optional(b[k], stringMap)) &&
        ['fillLayer', 'priorityTo'].every(k => optional(b[k], ref)) && optional(b.priorityFrom, paths) &&
        optional(b.mobileCards, v => paths(v) && new Set(v).size === b.cards.length &&
          v.length === b.cards.length && v.every(r => b.cards.includes(r))) &&
        optional(b.groups, v => collection(v, pair => Array.isArray(pair) && pair.length === 2 && text(pair[0]) && paths(pair[1]), 128)) &&
        optional(b.stats, v => collection(v, pair => Array.isArray(pair) && pair.length === 2 && natural(pair[0]) && text(pair[1]), 12)) &&
        optional(b.captionPriority, v => collection(v, text, 32)))) return false;
  if (!collection(views, v => Array.isArray(v) && v.length === 2 && container(v[0]) && Object.hasOwn(boards, v[0]) && text(v[1]), 24) ||
      !views.length || !views.some(v => v[0] === 'root') || new Set(views.map(v => v[0])).size !== views.length ||
      !optional(data.peersus.legend, stringMap) || !collection(violations, v => isRecord(v) && text(v.message) && anchor(v.source))) return false;
  if (!collection(data.links, l => isRecord(l) && ref(l.a) && ref(l.b) && kind(l.k) && text(l.desc) &&
      collection(l.labels, text, 32) && l.labels.length > 0 && optional(l.board, container) && optional(l.caption, text) &&
      optional(l.structural, v => typeof v === 'boolean'))) return false;
  const connector = v => typeof v === 'string' && /^x:(?:in|out):n:/.test(v) && ref(v.replace(/^x:(?:in|out):/, ''));
  for (const [key, diagram] of Object.entries(diagrams)) {
    if (!container(key) || !isRecord(diagram) || !paths(diagram.cards) || !collection(diagram.connectors, connector, 8)) return false;
    const cardSet = new Set([...diagram.cards, ...diagram.connectors]);
    if (cardSet.size !== diagram.cards.length + diagram.connectors.length ||
        !collection(diagram.edges, edge => Array.isArray(edge) && edge.length === 3 && edge[0] !== edge[1] &&
          cardSet.has(edge[0]) && cardSet.has(edge[1]) && collection(edge[2], i => natural(i) && i < data.links.length) && edge[2].length > 0)) return false;
    const indices = v => collection(v, i => natural(i) && i < diagram.edges.length);
    if (diagram.layers !== undefined) {
      if (!collection(diagram.layers, layer => collection(layer, v => cardSet.has(v), 128), 128) ||
          !indices(diagram.reduced) || !indices(diagram.feedback)) return false;
      const ordered = diagram.layers.flat();
      if (ordered.length !== cardSet.size || new Set(ordered).size !== cardSet.size) return false;
    }
  }
  if (Object.keys(boards).some(key => !Object.hasOwn(diagrams, key) || boards[key].cards.some(r => !diagrams[key].cards.includes(r)))) return false;
  const step = s => isRecord(s) && ref(s.ref) && text(s.note);
  if (!collection(data.flows, f => isRecord(f) && identifier(f.id) && text(f.title) && text(f.description) &&
      collection(f.steps, step, 64) && f.steps.length > 0, 32) || !collection(data.tour, s => step(s) && text(s.title), 64)) return false;
  return optional(tourAction, a => isRecord(a) && text(a.title) && data.flows.some(f => f.id === a.flow));
}

function decodePublication(raw) {
  if (typeof raw !== 'string' || raw.length > MAX_PUBLIC_BYTES) return null;
  try {
    const data = JSON.parse(raw);
    return isPublication(data) ? {data, raw} : null;
  } catch {
    return null;
  }
}

async function fetchPublication(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 8000);
  try {
    const response = await fetch(url, {cache: 'no-cache', credentials: 'omit', redirect: 'error',
      referrerPolicy: 'no-referrer', signal: controller.signal});
    if (response.status !== 200 || Number(response.headers.get('content-length')) > MAX_PUBLIC_BYTES || !response.body) return null;
    const reader = response.body.getReader(), decoder = new TextDecoder('utf-8', {fatal: true});
    let bytes = 0;
    const chunks = [];
    for (;;) {
      const {done, value} = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > MAX_PUBLIC_BYTES) return null;
      chunks.push(decoder.decode(value, {stream: true}));
    }
    chunks.push(decoder.decode());
    return decodePublication(chunks.join(''));
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
    controller.abort();
  }
}

function openMapCache() {
  return new Promise(resolve => {
    let settled = false;
    const finish = db => {
      if (settled) { db?.close(); return; }
      settled = true;
      clearTimeout(timer);
      resolve(db);
    };
    const timer = setTimeout(() => finish(null), 450);
    try {
      const request = indexedDB.open('peersus-architecture', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('map');
      request.onsuccess = () => finish(request.result);
      request.onerror = request.onblocked = () => finish(null);
    } catch { finish(null); }
  });
}

function cacheTransaction(db, mode, work) {
  if (!db) return Promise.resolve(null);
  return new Promise(resolve => {
    try {
      const transaction = db.transaction('map', mode);
      let result;
      transaction.oncomplete = () => resolve(result);
      transaction.onabort = transaction.onerror = () => resolve(null);
      work(transaction.objectStore('map'), value => { result = value; });
    } catch { resolve(null); }
  });
}

// Claim the attempt across tabs; until a valid response arrives, use the failure interval.
function checkSlot(db, now, interval, retry, hadPublishedCopy) {
  return cacheTransaction(db, 'readwrite', (store, done) => {
    const last = store.get('checked'), successful = store.get('successful');
    successful.onsuccess = () => {
      const at = natural(last.result) ? last.result : 0;
      // Older pages only stored "checked". A valid cached publication is their evidence of success.
      const wasSuccessful = successful.result === undefined ? hadPublishedCopy : successful.result === at;
      if (at > 0 && at <= now && now - at < (wasSuccessful ? interval : retry)) {
        done({check: false, at});
        return;
      }
      store.put(now, 'checked');
      store.put(0, 'successful');
      done({check: true, at: now});
    };
  });
}

function saveCheckSuccess(db, at, raw) {
  return cacheTransaction(db, 'readwrite', (store, done) => {
    const last = store.get('checked');
    last.onsuccess = () => {
      if (last.result === at) {
        store.put(at, 'successful');
        if (raw !== undefined) store.put(raw, 'publication');
      }
      done(true);
    };
  });
}

async function showArchitecture() {
  const snapshotRequest = fetchPublication(publicationStatus.dataset.snapshot);
  const db = await openMapCache();
  let shown, mounted, controller, checked;
  const updateStatus = () => {
    const when = checked ? new Date(checked).toISOString().slice(0, 16).replace('T', ' ') + ' UTC' : 'not yet';
    const label = shown ? `Commit ${shown.data.source.commit.slice(0, 12)} · ${shown.data.source.date.slice(0, 10)} · ${shown.copy}` : 'Map data unavailable';
    publicationStatus.textContent = `${label} · GitHub checked: ${when}`;
    publicationStatus.hidden = false;
  };
  const render = (record, copy) => {
    const saved = mounted?.save();
    controller?.abort();
    controller = new AbortController();
    shown = {...record, copy};
    mounted = mountArchitecture(record.data, controller.signal, saved);
    updateStatus();
    return mounted.ready;
  };
  try {
    const cache = await cacheTransaction(db, 'readonly', (store, done) => {
      const raw = store.get('publication'), last = store.get('checked');
      last.onsuccess = () => done({raw: raw.result, checked: last.result});
    });
    checked = natural(cache?.checked) && cache.checked <= Date.now() ? cache.checked : 0;
    const saved = decodePublication(cache?.raw);
    if (saved) await render(saved, 'published by CI');
    const snapshot = await snapshotRequest;
    if (snapshot && (!shown || snapshot.data.source.date > shown.data.source.date)) await render(snapshot, 'shipped with the site');
    updateStatus();
    const interval = Number(publicationStatus.dataset.checkInterval);
    const retry = Number(publicationStatus.dataset.retryInterval);
    if (!Number.isSafeInteger(interval) || !Number.isSafeInteger(retry) || retry < 1 || retry >= interval) return;
    const now = Date.now();
    const slot = await checkSlot(db, now, interval, retry, Boolean(saved));
    checked = slot?.at ?? now;
    updateStatus();
    if (slot && !slot.check) return;
    const published = await fetchPublication(publicationStatus.dataset.live);
    if (!published) return;
    const usePublished = !shown || published.data.source.date >= shown.data.source.date;
    if (db) {
      const stored = await saveCheckSuccess(db, now, usePublished ? published.raw : undefined);
      // An otherwise valid check still succeeded if the payload exceeded the storage quota.
      if (!stored && usePublished) await saveCheckSuccess(db, now);
    }
    if (usePublished && published.data.source.commit !== shown?.data.source.commit) await render(published, 'published by CI');
  } finally {
    db?.close();
  }
}

showArchitecture();
