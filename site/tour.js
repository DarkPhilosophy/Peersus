"use strict";
// The home page's phone: Peersus's own screens (shots/, drawn by the application itself, see docs/website.md), one after
// another, as a finger goes through the app: the tab it would press is touched, then that screen comes. A card of the rail
// shows its screen at once and ends the tour. The note under the phone says what the shown picture is (its `data-note`, or
// the demo's note). Nothing moves with reduced motion, while the tab is hidden or while the phone is out of sight.
(window.peersusPages ||= []).push((signal) => {
  const root = document.getElementById("tour");
  if (!root) return;
  const shots = [...root.querySelectorAll(".tour-shot")];
  const cards = [...root.querySelectorAll(".rail-card")];
  const tap = root.querySelector(".tour-tap");
  const order = shots.map((shot) => shot.dataset.screen);
  const note = document.getElementById("tour-note");
  const demoNote = note ? note.textContent : "";
  // Where each screen is reached on the phone, in percent of the picture: the four tabs at the bottom, Settings top right,
  // and the Extensions tab that extensions' pages add on the right of the bottom bar.
  const REACH = { send: [12.6, 96.6], receive: [37.4, 96.6], peers: [62.4, 96.6], transfers: [87.4, 96.6], store: [92.5, 3.1],
    remote: [89.8, 96.3], monitor: [89.8, 96.3] };
  // How long a screen stays, and how long the touch shows before its screen comes.
  const STAY = 4200;
  const TOUCH = 420;
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  let at = 0;
  let playing = !calm.matches;
  let inView = true;
  let timer = 0;

  function show(index) {
    at = (index + order.length) % order.length;
    const screen = order[at];
    root.dataset.screen = screen;
    for (const shot of shots) {
      const on = shot.dataset.screen === screen;
      shot.classList.toggle("is-on", on);
      shot.setAttribute("aria-hidden", on ? "false" : "true");
      // A picture not on show yet is fetched now, so that it is there when its turn comes.
      if (on) shot.loading = "eager";
      if (on && note) note.textContent = shot.dataset.note || demoNote;
    }
    for (const card of cards) {
      const on = card.dataset.screen === screen;
      card.setAttribute("aria-current", on ? "true" : "false");
      // The bar under the card shown fills while it stays; restarted by taking the class away and back.
      card.classList.remove("is-playing");
      if (on && playing) {
        void card.offsetWidth;
        card.classList.add("is-playing");
      }
    }
  }

  function touch(screen) {
    const [x, y] = REACH[screen] || [50, 50];
    tap.style.setProperty("--x", `${x}%`);
    tap.style.setProperty("--y", `${y}%`);
    tap.classList.remove("is-on");
    void tap.offsetWidth;
    tap.classList.add("is-on");
  }

  function schedule() {
    clearTimeout(timer);
    if (!playing || !inView || document.hidden) return;
    timer = setTimeout(() => {
      const next = (at + 1) % order.length;
      touch(order[next]);
      timer = setTimeout(() => {
        show(next);
        schedule();
      }, TOUCH);
    }, STAY - TOUCH);
  }

  function stop() {
    playing = false;
    clearTimeout(timer);
    root.classList.add("is-stopped");
    for (const card of cards) card.classList.remove("is-playing");
  }

  for (const card of cards) {
    card.addEventListener("click", () => {
      stop();
      show(order.indexOf(card.dataset.screen));
    }, { signal });
  }
  // A finger or a pointer resting on the phone holds the screen it is on.
  root.addEventListener("pointerenter", () => clearTimeout(timer), { signal });
  root.addEventListener("pointerleave", schedule, { signal });
  root.addEventListener("focusin", () => clearTimeout(timer), { signal });
  root.addEventListener("focusout", schedule, { signal });
  document.addEventListener("visibilitychange", schedule, { signal });
  calm.addEventListener("change", () => { if (calm.matches) stop(); }, { signal });
  if ("IntersectionObserver" in window) {
    const watcher = new IntersectionObserver(([entry]) => {
      inView = entry.isIntersecting;
      schedule();
    }, { threshold: 0.35 });
    watcher.observe(root);
    signal.addEventListener("abort", () => watcher.disconnect());
  }
  signal.addEventListener("abort", () => clearTimeout(timer));
  if (!playing) root.classList.add("is-stopped");
  root.classList.add("is-live");
  show(0);
  schedule();
});
