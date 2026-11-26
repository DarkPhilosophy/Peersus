"use strict";
// The home page's phone: Peersus's screens on made-up devices and data. Its tabs, buttons and fields answer; Monitor reads
// made-up numbers that move every second. Nothing on this page reads a file, a camera, a device or the network.
// The page that shows it (the site's home) asks for a screen; the phone's pointer travels to that tab and presses it, then
// the phone says which screen it shows, and says when a person pressed something, so that the page's tour can rest.
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const phone = $("#phone");
  const center = document.body;
  const views = $$(".view");
  const tabs = $$(".tab");
  const top = $(".top");
  const chooser = $("#pageChooser");
  const menu = $("#pageMenu");
  const cursor = $("#demoCursor");
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  const parentPage = window.parent !== window ? window.parent : null;
  const tell = (kind, name) => parentPage?.postMessage({ peersus: kind, name }, location.origin);
  let shown = "send";
  let extension = "remote";
  // The made-up devices: one list for Send, Peers, Remote and Monitor.
  const DEVICES = [
    { name: "Alice’s laptop", state: "online", cpu: "Intel Core i7-1360P", gpu: "Iris Xe", memory: 16, storage: 512, history: ["Received · notes.md · cancelled", "Sent · slides-final.pdf · done"] },
    { name: "Office desktop", state: "sleeping", cpu: "AMD Ryzen 7 7700", gpu: "Radeon RX 7600", memory: 32, storage: 1024, history: ["Sent · song-demo.flac · failed"] },
    { name: "Kitchen tablet", state: "online", cpu: "QTI SM8650", gpu: "Adreno 750", memory: 11, storage: 222.1, history: ["Received · recipes.txt · done"] },
  ];

  // The phone is drawn at its logical size and zoomed to the frame; positions inside it are in its own pixels.
  const fit = () => { center.style.zoom = String(Math.min(innerWidth / 432, innerHeight / 935)); };
  addEventListener("resize", fit);
  fit();
  const zoom = () => parseFloat(center.style.zoom) || 1;

  function show(name) {
    shown = name;
    const page = name === "remote" || name === "monitor" ? "extensions" : name;
    if (page === "extensions") extension = name;
    if (name !== "settings") $("#settingsSearch").value = "";
    for (const view of views) view.hidden = view.dataset.view !== name;
    for (const tab of tabs) tab.setAttribute("aria-current", tab.dataset.tab === page ? "true" : "false");
    top.classList.toggle("ext", page === "extensions");
    chooser.hidden = page !== "extensions";
    $("#pageName").textContent = extension === "remote" ? "Remote" : "Monitor";
    for (const mark of $$(".chooser-icon")) mark.hidden = mark.dataset.for !== extension;
    menu.hidden = true;
    $(".views").scrollTop = 0;
    tell("shown", name);
  }

  // The pointer: a curved path to the tab, a press, the screen, then it fades.
  const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
  const bezier = (a, b, c, d, t) => { const u = 1 - t; return u * u * u * a + 3 * u * u * t * b + 3 * u * t * t * c + t * t * t * d; };
  let at = null;
  let travel = 0;
  function aim(element) {
    const box = phone.getBoundingClientRect();
    const target = (element.querySelector("svg") || element).getBoundingClientRect();
    return { x: (target.left + target.width * 0.52 - box.left) / zoom(), y: (target.top + target.height * 0.52 - box.top) / zoom() };
  }
  function paint(point, turn = 0) {
    at = point;
    cursor.classList.add("visible");
    cursor.style.transform = `translate3d(${point.x - 6}px, ${point.y - 6}px, 0) rotate(${turn}deg)`;
  }
  async function press(element, then) {
    const token = ++travel;
    if (calm.matches) { then(); return; }
    const end = aim(element);
    const start = at || { x: end.x - 90, y: end.y - 150 };
    const dx = end.x - start.x, dy = end.y - start.y;
    const distance = Math.hypot(dx, dy);
    const arc = Math.min(105, Math.max(22, distance * 0.27));
    const side = dx >= 0 ? 1 : -1;
    const c1 = { x: start.x + dx * 0.28 + side * 8, y: start.y + dy * 0.26 - arc };
    const c2 = { x: start.x + dx * 0.73 - side * 12, y: end.y - dy * 0.22 - arc * 0.82 };
    const duration = Math.max(380, Math.min(720, 360 + distance * 0.9));
    const began = performance.now();
    await new Promise((resolve) => {
      const frame = (now) => {
        if (token !== travel) { resolve(); return; }
        const t = Math.min(1, (now - began) / duration), e = ease(t);
        paint({ x: bezier(start.x, c1.x, c2.x, end.x, e), y: bezier(start.y, c1.y, c2.y, end.y, e) },
          Math.sin(Math.PI * e) * Math.min(10, Math.max(3, distance * 0.035)) * side);
        if (t < 1) requestAnimationFrame(frame); else resolve();
      };
      requestAnimationFrame(frame);
    });
    if (token !== travel) return;
    cursor.classList.remove("is-clicking"); void cursor.offsetWidth; cursor.classList.add("is-clicking");
    element.classList.add("is-demo-pressed");
    await new Promise((resolve) => setTimeout(resolve, 120));
    then();
    await new Promise((resolve) => setTimeout(resolve, 180));
    element.classList.remove("is-demo-pressed");
    cursor.classList.remove("is-clicking", "visible");
    at = null;
  }

  // A screen asked for by the page: through the tab bar, and for an extension's page through its chooser.
  async function go(name) {
    const page = name === "remote" || name === "monitor" ? "extensions" : name;
    const tab = tabs.find((t) => t.dataset.tab === page);
    if (page === "extensions" && shown !== "remote" && shown !== "monitor") {
      await press(tab, () => show(extension));
    } else if (page !== "extensions" && shown !== name) {
      await press(tab, () => show(name));
    }
    if (page === "extensions" && shown !== name) await press(chooser, () => show(name));
  }

  for (const tab of tabs) tab.addEventListener("click", () => show(tab.dataset.tab === "extensions" ? extension : tab.dataset.tab));
  chooser.addEventListener("click", (event) => { event.stopPropagation(); menu.hidden = !menu.hidden; });
  for (const item of $$("[data-page]", menu)) item.addEventListener("click", () => show(item.dataset.page));
  document.addEventListener("click", (event) => { if (!menu.contains(event.target)) menu.hidden = true; });
  // Settings: opened by the gear over whatever was shown, and closed back to it.
  let before = "send";
  $("#gear").addEventListener("click", () => { if (shown !== "settings") { before = shown; show("settings"); } });
  $("#settingsClose").addEventListener("click", () => show(before));
  for (const group of $$("[data-choice]")) {
    for (const chip of $$(".chip", group)) chip.addEventListener("click", () => {
      for (const other of $$(".chip", group)) other.classList.toggle("on", other === chip);
      if (group.dataset.choice === "renderer") {
        $("#rendererSaved").textContent = `Saved preference: ${chip.textContent}`;
        note($("#settingsToast"), "Used after the next start, as in the application.");
      }
    });
  }
  for (const swatch of $$(".swatch")) {
    swatch.style.setProperty("--swatch", swatch.dataset.accent);
    swatch.addEventListener("click", () => {
      for (const other of $$(".swatch")) other.classList.toggle("on", other === swatch);
      document.documentElement.style.setProperty("--coral", swatch.dataset.accent);
    });
  }
  $("#settingsSearch").addEventListener("input", (event) => {
    const query = event.target.value.trim().toLowerCase();
    for (const part of $$("[data-view=settings] .caps, [data-view=settings] .sub, [data-view=settings] .section-title")) {
      part.classList.toggle("dim", query !== "" && !part.textContent.toLowerCase().includes(query));
    }
  });


  const el = (tag, className, text) => { const e = document.createElement(tag); if (className) e.className = className; if (text != null) e.textContent = text; return e; };
  const who = (device) => {
    const box = el("span", "who");
    box.append(el("b", null, device.name), el("small", `state ${device.state}`, device.state.toUpperCase()));
    return box;
  };
  for (const device of DEVICES) {
    const card = el("button", "card device");
    card.type = "button"; card.dataset.device = device.name;
    card.append(el("span", "avatar", device.name[0]), who(device));
    $("#savedDevices").append(card);
    const peer = el("div", "card peer");
    peer.dataset.device = device.name;
    const row = el("div", "row");
    row.append(el("span", "avatar", device.name[0]), who(device));
    peer.append(row);
    if (device.state === "online") {
      const actions = el("div", "actions");
      for (const [key, label] of [["send", "Send"], ["forget", "Forget"], ["test", "Test connection"], ["where", "Where is this device?"]]) {
        const button = el("button", "chip", label); button.type = "button"; button.dataset.peer = key; actions.append(button);
      }
      peer.append(actions);
    }
    peer.append(el("p", "caps", "HISTORY"));
    for (const line of device.history) peer.append(el("p", "history", line));
    const toast = el("p", "toast"); toast.hidden = true; peer.append(toast);
    $("#pairedPeers").append(peer);
  }

  // A short answer under what was pressed; a demo never pretends to do what it cannot.
  function note(element, text, page) {
    if (page && shown !== page) show(page);
    element.textContent = text;
    element.hidden = false;
    clearTimeout(element.timer);
    element.timer = setTimeout(() => { element.hidden = true; }, 2600);
  }

  // Send.
  let chosen = null;
  for (const device of $$("#savedDevices .device")) {
    device.addEventListener("click", () => {
      chosen = device.dataset.device;
      for (const other of $$("#savedDevices .device")) other.classList.toggle("selected", other === device);
      note($("#sendToast"), `${chosen} chosen. Hold the portal to send; this model reads no files.`);
    });
  }
  $("#sendPortal").addEventListener("click", () => note($("#sendToast"), chosen ? `Files would go to ${chosen} once it says yes.` : "Pick a saved device first; this model reads no files."));

  // Receive.
  $("#receiveJoin").addEventListener("click", () => {
    const code = $("#receiveCode").value.trim();
    note($("#receiveToast"), code ? "Joined with that code. Nothing lands until you accept it." : "Paste the code the other device shows.");
  });
  $("#receivePortal").addEventListener("click", () => $("#receiveCode").focus());

  // Peers.
  let nearby = false;
  $("#nearbyToggle").addEventListener("click", () => {
    nearby = !nearby;
    $("#nearbyList").hidden = !nearby;
    $("#nearbyText").textContent = nearby ? "Devices nearby are on. These can be paired." : "Devices nearby are off. Turn them on to see devices close to you that can be paired.";
    $("#nearbyToggle").lastChild.textContent = nearby ? "Turn off devices nearby" : "Turn on devices nearby";
  });
  const peerSays = { send: "Choose files for this device on Send.", forget: "Forgetting asks first; this model keeps it.", test: "Connection tested: 38 ms, direct.", where: "Shown on the map of the Location extension." };
  for (const button of $$(".peer [data-peer]")) {
    button.addEventListener("click", () => {
      const name = button.closest(".peer").dataset.device;
      // Send goes to Send with this device chosen, as the application does.
      if (button.dataset.peer === "send") { show("send"); $$("#savedDevices .device").find((d) => d.dataset.device === name)?.click(); return; }
      note(button.closest(".peer").querySelector(".toast"), peerSays[button.dataset.peer]);
    });
  }
  $("#pairButton").addEventListener("click", () => note($("#nearbyText"), "Pair: both people pick the same number of three; it is read, never placed."));
  $("#connectButton").addEventListener("click", () => note($("#nearbyText"), "Connect: scan or paste the code the other device shows."));

  // Transfers: the moving ones move, the filters and the search filter, Cancel cancels.
  const items = $$("#transferList .transfer");
  let filter = "all";
  function filterList() {
    const query = $("#transferSearch").value.trim().toLowerCase();
    for (const item of items) {
      const kind = item.className;
      const wanted = filter === "all" || (filter === "active" && item.dataset.progress && !kind.includes("cancelled"))
        || (filter === "done" && kind.includes("done")) || (filter === "stopped" && /failed|cancelled/.test(kind))
        || (filter === "incoming" && kind.includes("incoming"));
      item.hidden = !(wanted && item.querySelector("b").textContent.toLowerCase().includes(query));
    }
  }
  for (const chip of $$(".filters .chip")) {
    chip.addEventListener("click", () => {
      filter = chip.dataset.filter;
      for (const other of $$(".filters .chip")) other.classList.toggle("on", other === chip);
      filterList();
    });
  }
  $("#transferSearch").addEventListener("input", filterList);
  $("#clearDone").addEventListener("click", () => {
    for (const item of items) if (/done|failed|cancelled/.test(item.className) && !item.dataset.progress) item.hidden = true;
  });
  for (const transfer of $$(".transfer[data-progress]")) {
    transfer.querySelector(".cancel").addEventListener("click", () => {
      delete transfer.dataset.progress;
      transfer.className = "card transfer small incoming cancelled";
      transfer.querySelector(".state").textContent = "INCOMING · CANCELLED";
      transfer.querySelector(".state").className = "state mutedstate";
      transfer.querySelector(".bar")?.remove();
      transfer.querySelector(".cancel").remove();
    });
  }
  for (const transfer of $$(".transfer[data-progress]")) transfer.querySelector(".bar").style.setProperty("--p", `${transfer.dataset.progress}%`);
  setInterval(() => {
    if (document.hidden) return;
    for (const transfer of $$(".transfer[data-progress]")) {
      const p = Math.min(99, Number(transfer.dataset.progress) + Math.random() * 1.6);
      transfer.dataset.progress = String(p);
      transfer.querySelector(".bar").style.setProperty("--p", `${p}%`);
      transfer.querySelector(".bar em").textContent = `${p.toFixed(2)}%`;
      transfer.querySelector(".pct").textContent = String(Math.floor(p));
    }
  }, 1000);

  // Remote: the keys press, the wheel rolls, the pad follows a finger.
  for (const key of $$(".key")) key.addEventListener("click", () => { key.classList.add("pressed"); setTimeout(() => key.classList.remove("pressed"), 170); });
  $(".wheel").addEventListener("click", (event) => { const wheel = event.currentTarget; wheel.classList.remove("rolling"); void wheel.offsetWidth; wheel.classList.add("rolling"); });
  const pad = $("#pad");
  const dot = $("#padPointer");
  pad.addEventListener("pointermove", (event) => {
    const box = pad.getBoundingClientRect();
    dot.style.left = `${(event.clientX - box.left) / zoom()}px`;
    dot.style.top = `${(event.clientY - box.top) / zoom()}px`;
    pad.classList.add("on");
  });
  pad.addEventListener("pointerleave", () => pad.classList.remove("on"));

  // Monitor: made-up readings that move every second.
  const history = { cpu: Array.from({ length: 40 }, () => 6 + Math.random() * 8), gpu: Array.from({ length: 40 }, () => Math.random() * 3) };
  const state = { cpu: 7, gpu: 0, memory: 56, storage: 25 };
  const walk = (value, step, low, high) => Math.min(high, Math.max(low, value + (Math.random() - 0.5) * step));
  function spark(card, values) {
    const max = 100;
    const points = values.map((v, i) => `${(i / (values.length - 1)) * 100},${40 - (v / max) * 40 * 2.4}`);
    card.querySelector(".line").setAttribute("d", `M${points.join("L")}`);
    card.querySelector(".area").setAttribute("d", `M0,40L${points.join("L")}L100,40Z`);
  }
  function ring(card, percent, amount) {
    card.querySelector(".val").style.strokeDasharray = `${(percent / 100) * 188.5} 251.3`;
    card.querySelector(".pct").textContent = `${Math.round(percent)}%`;
    card.querySelector(".amount").textContent = amount;
  }
  function read() {
    state.cpu = walk(state.cpu, 9, 2, 46);
    state.gpu = Math.random() < 0.12 ? 20 + Math.random() * 30 : walk(state.gpu, 2, 0, 8);
    state.memory = walk(state.memory, 1.4, 48, 64);
    state.storage = walk(state.storage, 0.05, 24.8, 25.4);
    for (const name of ["cpu", "gpu"]) {
      history[name].push(state[name]);
      history[name].shift();
      const card = $(`#${name}`);
      card.querySelector(".pct").textContent = `${Math.round(state[name])}%`;
      card.querySelector(".clock").textContent = name === "cpu" ? `${Math.round(600 + state.cpu * 40)} MHz` : `${Math.round(300 + state.gpu * 12)} MHz`;
      card.querySelector(".extra").textContent = name === "cpu" ? "8 cores" : `${(0.4 + state.gpu / 40).toFixed(1)} GiB`;
      card.querySelector(".t").textContent = `${Math.round(30 + state[name] / 6)}°C`;
      card.querySelector(".bar i").style.setProperty("--p", `${state[name]}%`);
      spark(card, history[name]);
    }
    ring($("#memory"), state.memory, `${(target.memory * state.memory / 100).toFixed(1)} / ${target.memory.toFixed(1)} GiB`);
    ring($("#storage"), state.storage, `${(target.storage * state.storage / 100).toFixed(1)} / ${target.storage.toFixed(1)} GiB`);
  }
  // One list of devices for every screen (the same as Send's and Peers'); Remote and Monitor share the chosen one.
  let target = DEVICES[0];
  const deviceMenu = $("#deviceMenu");
  function chip(button) {
    button.innerHTML = "";
    const dot = document.createElement("i"); dot.className = `dot ${target.state === "online" ? "green" : "violet"}`;
    const avatar = document.createElement("span"); avatar.className = "avatar s"; avatar.textContent = target.name[0];
    const state = document.createElement("small"); state.className = `state ${target.state}`; state.textContent = target.state.toUpperCase();
    button.append(dot, avatar, document.createTextNode(` ${target.name} `), state, document.createTextNode(" ▾"));
  }
  function choose(device) {
    target = device;
    for (const button of $$("[data-devices]")) chip(button);
    $("[data-part=cpu]").textContent = device.cpu;
    $("[data-part=gpu]").textContent = device.gpu;
    const asleep = device.state !== "online";
    $(".mouse").classList.toggle("off", asleep);
    $("#remoteAsleep").hidden = !asleep;
    $("#remoteAsleep").textContent = `${device.name} is sleeping. Its status can be read on Monitor; it can be driven once it is online.`;
    $("#lastReading").hidden = !asleep;
    $("#lastReading").textContent = "SLEEPING · LAST READING, 12 MIN AGO";
    $(".grid").classList.toggle("still", asleep);
    reseed();
  }
  for (const device of DEVICES) {
    const item = document.createElement("button");
    item.type = "button"; item.setAttribute("role", "option");
    const avatar = document.createElement("span"); avatar.className = "avatar s"; avatar.textContent = device.name[0];
    const state = document.createElement("small"); state.className = `state ${device.state}`; state.textContent = device.state.toUpperCase();
    item.append(avatar, document.createTextNode(device.name), state);
    item.addEventListener("click", (event) => {
      event.stopPropagation();
      choose(device);
      deviceMenu.hidden = true;
    });
    deviceMenu.append(item);
  }
  for (const button of $$("[data-devices]")) {
    button.addEventListener("click", (event) => {
      event.stopPropagation();
      if (!deviceMenu.hidden) { deviceMenu.hidden = true; return; }
      const box = button.getBoundingClientRect(), frame = phone.getBoundingClientRect();
      const below = (box.bottom - frame.top) / zoom() + 6, above = (box.top - frame.top) / zoom() - 6;
      deviceMenu.style.left = `${(box.left - frame.left) / zoom()}px`;
      deviceMenu.hidden = false;
      // Under the chip when it fits, else over it (Remote's chip sits low on the screen).
      deviceMenu.style.top = below + deviceMenu.offsetHeight < 915 - 90 ? `${below}px` : `${above - deviceMenu.offsetHeight}px`;
      for (const item of deviceMenu.children) item.setAttribute("aria-selected", item.textContent.startsWith(target.name) ? "true" : "false");
    });
  }
  document.addEventListener("click", () => { deviceMenu.hidden = true; });
  function reseed() {
    state.cpu = 4 + Math.random() * 20; state.gpu = Math.random() * 6; state.memory = 35 + Math.random() * 30; state.storage = 15 + Math.random() * 50;
    history.cpu = Array.from({ length: 40 }, () => state.cpu * (0.6 + Math.random() * 0.8));
    history.gpu = Array.from({ length: 40 }, () => Math.random() * 3);
    read();
  }
  choose(target);
  setInterval(() => { if (!document.hidden && shown === "monitor" && target.state === "online") read(); }, 1000);

  // What the page asks, and what a person does.
  let asking = false;
  addEventListener("message", async (event) => {
    if (event.origin !== location.origin || event.data?.peersus !== "show") return;
    asking = true;
    await go(String(event.data.name));
    asking = false;
  });
  document.addEventListener("pointerdown", () => { if (!asking) { travel++; cursor.classList.remove("visible"); tell("touched"); } }, true);
  document.addEventListener("keydown", () => { if (!asking) tell("touched"); }, true);
  show("send");
  tell("ready");
})();
