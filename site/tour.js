"use strict";
// The home page's phone: an interactive model of Peersus (demo/phone.html) on made-up devices and data. The page goes
// through its screens one after another, the model's own pointer moving to each tab and pressing it; the bar along the top
// of the shown card fills while that screen stays. A card shows its screen at once; anything a person presses inside the
// phone is theirs. After either, the tour rests, then goes on from the screen shown. Nothing moves with reduced motion,
// while the tab is hidden or the phone is out of sight; a pointer resting on the phone holds the screen it is on.
(window.peersusPages ||= []).push((signal) => {
  const root = document.getElementById("tour");
  const phone = document.getElementById("tour-phone");
  if (!root || !phone) return;
  const cards = [...root.querySelectorAll(".rail-card")];
  const order = cards.map((card) => card.dataset.screen);
  // How long a screen stays, and how long the tour rests after a person chose a screen or used the phone.
  const STAY = 4200;
  const REST = 9000;
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  let at = 0;
  let ready = false;
  let inView = true;
  let held = false;
  let timer = 0;

  const running = () => ready && !calm.matches && inView && !held && !document.hidden;
  const ask = (name) => phone.contentWindow?.postMessage({ peersus: "show", name }, location.origin);

  // What the phone shows is what the rail marks; the bar of that card fills for as long as it stays.
  function mark(name, stay) {
    const index = order.indexOf(name);
    if (index < 0) return;
    at = index;
    root.style.setProperty("--stay", `${stay}ms`);
    for (const card of cards) {
      const on = card.dataset.screen === name;
      card.setAttribute("aria-current", on ? "true" : "false");
      card.classList.remove("is-playing");
      if (on && running()) {
        void card.offsetWidth;
        card.classList.add("is-playing");
      }
    }
  }

  function schedule(wait = STAY) {
    clearTimeout(timer);
    root.classList.toggle("is-paused", !running());
    if (!running()) return;
    timer = setTimeout(() => ask(order[(at + 1) % order.length]), wait);
  }

  let resting = false;
  function rest() {
    resting = true;
    const card = cards[at];
    if (card) mark(card.dataset.screen, REST);
    schedule(REST);
  }

  window.addEventListener("message", (event) => {
    if (event.origin !== location.origin || event.source !== phone.contentWindow || !event.data) return;
    const { peersus: kind, name } = event.data;
    if (kind === "ready") {
      ready = true;
      mark(order[at], STAY);
      schedule();
    } else if (kind === "shown") {
      const stay = resting ? REST : STAY;
      resting = false;
      mark(name, stay);
      schedule(stay);
    } else if (kind === "touched") {
      rest();
    }
  }, { signal });

  for (const card of cards) {
    card.addEventListener("click", () => {
      resting = true;
      ask(card.dataset.screen);
      schedule(REST);
    }, { signal });
  }
  // A pointer resting on the phone holds the screen it is on; leaving lets the tour go on.
  const hold = (on) => () => {
    held = on;
    mark(order[at], resting ? REST : STAY);
    schedule();
  };
  phone.addEventListener("pointerenter", hold(true), { signal });
  phone.addEventListener("pointerleave", hold(false), { signal });
  document.addEventListener("visibilitychange", () => schedule(), { signal });
  calm.addEventListener("change", () => schedule(), { signal });
  if ("IntersectionObserver" in window) {
    const watcher = new IntersectionObserver(([entry]) => {
      inView = entry.isIntersecting;
      schedule();
    }, { threshold: 0.35 });
    watcher.observe(root);
    signal.addEventListener("abort", () => watcher.disconnect());
  }
  // The phone may have said it is ready before this page listened.
  const loaded = () => { ready = true; mark(order[at], STAY); schedule(); };
  if (phone.contentDocument?.readyState === "complete") loaded();
  phone.addEventListener("load", loaded, { signal });
  signal.addEventListener("abort", () => clearTimeout(timer));
  root.classList.add("is-live");
});
