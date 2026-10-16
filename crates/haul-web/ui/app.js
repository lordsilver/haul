// Haul web UI. Renders the state the engine pushes over /api/events and sends
// the user's actions to the JSON API. List rows and host cards are updated in
// place, so a link being typed into a row survives the live updates.

import { ICONS } from "/icons.js";

const $ = (id) => document.getElementById(id);
const SVG = "http://www.w3.org/2000/svg";

function icon(name, className = "icon") {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 20 20");
  svg.setAttribute("class", className);
  svg.setAttribute("aria-hidden", "true");
  for (const d of ICONS[name] ?? []) {
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", d);
    svg.append(path);
  }
  return svg;
}

function el(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? "" : value);
  }
  node.append(...children.flat().filter((c) => c !== null && c !== undefined && c !== false));
  return node;
}

// replaceChildren without the sections a condition left out.
function fill(node, ...children) {
  node.replaceChildren(...children.flat().filter((c) => c !== null && c !== undefined && c !== false));
}

function setText(node, text) {
  const value = text ?? "";
  if (node.textContent !== value) node.textContent = value;
}

function button(label, { icon: name, kind = "", title, onclick, iconOnly = false, disabled = false } = {}) {
  return el(
    "button",
    {
      type: "button",
      class: `button ${kind}${iconOnly ? " icon-only" : ""}`.trim(),
      title: title ?? (iconOnly ? label : undefined),
      "aria-label": iconOnly ? label : undefined,
      disabled,
      onclick,
    },
    name ? icon(name) : null,
    iconOnly ? null : el("span", { class: name ? "command-label" : undefined }, label),
  );
}

const UNITS = [
  [1e12, "TB", 2],
  [1e9, "GB", 2],
  [1e6, "MB", 1],
  [1e3, "KB", 0],
];

function bytes(n) {
  for (const [size, unit, digits] of UNITS) if (n >= size) return `${(n / size).toFixed(digits)} ${unit}`;
  return `${Math.round(n)} B`;
}

function duration(seconds) {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

const isWebLink = (url) => /^https?:\/\//i.test(url);

// The host a row names: the one in use, else the first mirror's.
// The host a row downloads from, or before it starts the one it would try
// first: the best preferred host it has, else its first host that is on.
const shownHost = (it) => {
  if (it.host) return it.host;
  const hosts = new Map((state?.hosts ?? []).map((h) => [h.name, h]));
  const on = it.mirrors.filter((m) => !hosts.get(m)?.disabled);
  const rank = (m) => hosts.get(m)?.preferred ?? Infinity;
  return [...on].sort((a, b) => rank(a) - rank(b))[0] || it.mirrors[0] || "";
};

// Errors arrive as "host: message"; the host is on screen already.
function withoutHost(message, host) {
  return host && message.startsWith(`${host}: `) ? message.slice(host.length + 2) : message;
}

// localStorage can be missing or throw (private windows, blocked site data).
const prefs = {
  get(key) {
    try {
      return localStorage.getItem(`haul.${key}`);
    } catch {
      return null;
    }
  },
  set(key, value) {
    try {
      localStorage.setItem(`haul.${key}`, value);
    } catch {}
  },
};

async function api(path, body) {
  const init = { method: "POST" };
  if (body instanceof FormData) {
    init.body = body;
  } else {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body ?? {});
  }
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || `${res.status} ${res.statusText}`);
  return data;
}

// ---- toasts

// A toast can offer one action: a button (`onclick`) or a link (`href` opens
// it). One that offers something stays longer, to be read and used.
function toast(message, kind = "info", action = null) {
  const names = { info: "info", success: "checkmark-circle", error: "error-circle" };
  const body = el("div", { class: "toast-body" }, el("div", {}, message));
  const node = el("div", { class: `toast ${kind}` }, icon(names[kind] ?? "info"), body);
  if (action) {
    const run = (e) => {
      e.stopPropagation();
      const close = () => node.remove();
      // A link opens on its own; a button's work may take a moment.
      Promise.resolve(action.onclick?.()).then(close, close);
    };
    body.append(
      action.href
        ? el(
            "a",
            { class: "button accent small", href: action.href, target: "_blank", rel: "noopener noreferrer", onclick: run },
            action.icon ? icon(action.icon) : null,
            el("span", { class: "command-label" }, action.label),
          )
        : button(action.label, { icon: action.icon, kind: "accent small", onclick: run }),
    );
  }
  node.addEventListener("click", () => node.remove());
  const box = $("toasts");
  box.append(node);
  while (box.childElementCount > 4) box.firstElementChild.remove();
  setTimeout(() => node.remove(), action ? 12000 : kind === "error" ? 7000 : 4000);
}

const report = (error) => toast(error.message, "error");
const act = (id, action) => api(`/api/item/${encodeURIComponent(id)}/${action}`).catch(report);
// The in-app browser window for a page that needs a person.
const openHere = (id) => api(`/api/item/${encodeURIComponent(id)}/browse`, { user_agent: navigator.userAgent }).catch(report);

function addedMessage(r) {
  const parts = [`${r.added} added`];
  if (r.merged) parts.push(`${r.merged} merged as mirrors`);
  if (r.duplicates) parts.push(`${r.duplicates} already in the list`);
  if (r.invalid) parts.push(`${r.invalid} not http(s)`);
  return parts.join(" · ");
}

// ---- views

const ACTIVE = ["downloading", "resolving", "waiting", "queued", "checking"];

// Paused before any byte came: what a wrapper's files are until started.
const notStarted = (i) => i.status === "paused" && !i.done;

const LISTS = {
  all: { label: "All downloads", icon: "apps-list", test: () => true },
  active: { label: "Active", icon: "arrow-circle-down", test: (i) => ACTIVE.includes(i.status) },
  attention: { label: "Needs you", icon: "warning", test: (i) => i.status === "captcha", badge: "attention" },
  failed: { label: "Failed", icon: "error-circle", test: (i) => i.status === "error", badge: "critical" },
  paused: { label: "Paused", icon: "pause-circle", test: (i) => i.status === "paused" },
  done: { label: "Completed", icon: "checkmark-circle", test: (i) => i.status === "done" },
};

// Wrapper links, below the lists: what each held, or why it did not open.
const SECTIONS = {
  wrappers: { label: "Wrappers", icon: "link" },
};

const PAGES = {
  hosts: { label: "Hosts", icon: "server" },
  settings: { label: "Settings", icon: "settings" },
};

const EMPTY = {
  active: ["Nothing is downloading", "Downloads that are running or waiting their turn show up here."],
  attention: ["No host is waiting for you", "When a host wants a captcha or a browser step, the download waits here."],
  failed: ["No failed downloads", "Downloads whose mirrors all failed show up here."],
  paused: ["Nothing is paused", "Paused downloads keep their progress and resume where they stopped."],
  done: ["No completed downloads yet", "Finished files show up here until you clear them."],
};

let state = null;
let view = "all";
let query = "";
let offline = false;
// The version that served this page, and whether another one answers now.
let pageVersion = null;
let updated = false;

function currentView() {
  const name = location.hash.replace(/^#\/?/, "");
  return name in LISTS || name in SECTIONS || name in PAGES ? name : "all";
}

// #/add/<links> opens the Add dialog with the links filled in (a bookmarklet
// can send the page it is on); adding still takes the user's click.
function takeAddRoute() {
  const match = location.hash.match(/^#\/add(?:\/(.*))?$/s);
  if (!match) return;
  history.replaceState(null, "", "#/all");
  let text = "";
  try {
    text = decodeURIComponent(match[1] ?? "");
  } catch {}
  openAdd(text);
}

function go(name) {
  location.hash = `#/${name}`;
}

addEventListener("hashchange", () => {
  takeAddRoute();
  const next = currentView();
  if (next !== view) selected.clear();
  view = next;
  if (state) render(state);
});

// ---- theme

let theme = prefs.get("theme") ?? "system";

function applyTheme(next) {
  theme = next;
  const root = document.documentElement;
  if (next === "light" || next === "dark") root.dataset.theme = next;
  else delete root.dataset.theme;
  prefs.set("theme", next);
}

function setTheme(next) {
  applyTheme(next);
  if (state) render(state);
  api("/api/settings", { theme: next }).catch(report);
}

const THEMES = [
  ["system", "System", "desktop"],
  ["light", "Light", "weather-sunny"],
  ["dark", "Dark", "weather-moon"],
];

// ---- calls for attention

// Downloads that entered the "Needs you" state, gathered for a moment so a
// burst shares one toast. The first state after opening Haul raises nothing:
// those downloads were already parked where they are.
let seenStatus = null;
const needsPending = new Map();
let needsTimer = null;

// A wrapper waits for a person like a download does, with its own name and
// its own section to land on.
function waitingNow(s) {
  return [
    ...s.items.filter((it) => it.status === "captcha"),
    ...s.wrappers.filter((w) => w.status === "captcha").map((w) => ({ id: w.id, name: wrapperName(w), open_urls: w.open_urls, wrapper: true })),
  ];
}

function spotNeedsUser(s) {
  const statuses = new Map([...s.items, ...s.wrappers].map((it) => [it.id, it.status]));
  if (seenStatus) {
    for (const it of waitingNow(s)) {
      if (seenStatus.get(it.id) !== "captcha") needsPending.set(it.id, it);
    }
    if (needsPending.size && !needsTimer) {
      needsTimer = setTimeout(() => {
        needsTimer = null;
        // One resolved in the moment since (a pasted link, a try) is not
        // announced after the fact.
        const still = new Set(state ? waitingNow(state).map((it) => it.id) : []);
        const items = [...needsPending.values()].filter((it) => still.has(it.id));
        needsPending.clear();
        if (items.length) needsToast(items);
      }, 1200);
    }
    // A wrapper just unwrapped: its files wait for a start.
    for (const w of s.wrappers) {
      const before = seenStatus.get(w.id);
      if (w.status === "done" && before && before !== "done") unwrappedToast(w, s);
    }
  }
  seenStatus = statuses;
}

function needsToast(items) {
  const one = items.length === 1 ? items[0] : null;
  const place = items.every((it) => it.wrapper) ? "wrappers" : "attention";
  let action = null;
  if (one && state?.desktop) {
    action = { label: "Open here", icon: "globe", onclick: () => openHere(one.id) };
  } else if (one) {
    const url = one.open_urls.find(isWebLink);
    // Opens the page in a tab of its own; Haul lands on the list that waits,
    // ready for the link the page hands back.
    if (url) action = { label: "Open page", icon: "open", href: url, onclick: () => go(place) };
  }
  action ??= { label: "Show", icon: "warning", onclick: () => go(place) };
  toast(one ? `${one.name} needs you` : `${items.length} ${place === "wrappers" ? "wrappers" : "downloads"} need you`, "info", action);
}

function unwrappedToast(w, s) {
  const waiting = s.items.filter((i) => i.source === w.id && i.status === "paused").length;
  const action = waiting ? { label: "Start all", icon: "play", onclick: () => startWrapper(w) } : { label: "Show", icon: "link", onclick: () => go("wrappers") };
  toast(waiting ? `${wrapperName(w)}: ${plural(waiting, "file")} ready to start` : `${wrapperName(w)} is unwrapped`, "info", action);
}

// ---- render

function render(s) {
  state = s;
  wrapperNames = new Map(s.wrappers.map((w) => [w.id, wrapperName(w)]));
  if (s.theme !== theme) applyTheme(s.theme);
  spotNeedsUser(s);
  renderNav(s);
  renderHead(s);
  const isList = view in LISTS;
  $("view-downloads").hidden = !isList;
  $("view-wrappers").hidden = view !== "wrappers";
  $("view-hosts").hidden = view !== "hosts";
  $("view-settings").hidden = view !== "settings";
  if (isList) {
    renderBanner(s);
    renderList(s);
  } else if (view === "wrappers") {
    renderWrappers(s);
  } else if (view === "hosts") {
    renderHosts(s);
  } else {
    renderSettings(s);
  }
  renderStatus(s);
  const title = s.speed ? `${bytes(s.speed)}/s · Haul` : "Haul";
  if (document.title !== title) document.title = title;
}

const navItems = new Map();

function navItem(key, spec) {
  const badge = el("span", { class: "badge" });
  const node = el(
    "button",
    { type: "button", class: "nav-item", title: spec.label, onclick: () => go(key) },
    icon(spec.icon),
    el("span", { class: "label" }, spec.label),
    badge,
  );
  return { node, badge, spec, filled: false };
}

function renderNav(s) {
  if (!navItems.size) {
    for (const [key, spec] of Object.entries({ ...LISTS, ...SECTIONS })) {
      const item = navItem(key, spec);
      navItems.set(key, item);
      $("nav").append(item.node);
    }
    for (const [key, spec] of Object.entries(PAGES)) {
      const item = navItem(key, spec);
      navItems.set(key, item);
      $("nav-footer").append(item.node);
    }
  }
  renderFolderNav(s);
  for (const [key, item] of navItems) {
    const selected = key === view;
    if (selected) item.node.setAttribute("aria-current", "page");
    else item.node.removeAttribute("aria-current");
    if (item.filled !== selected) {
      item.filled = selected;
      item.node.firstChild.replaceWith(icon(selected ? `${item.spec.icon}-filled` : item.spec.icon));
    }
    if (key === "wrappers") {
      // Counted like a list; it calls for attention while one waits or failed.
      const count = s.wrappers.length;
      const kind = s.wrappers.some((w) => w.status === "error") ? "critical" : s.wrappers.some((w) => w.status === "captcha") ? "attention" : "";
      setText(item.badge, count ? String(count) : "");
      item.badge.className = `badge${kind ? ` ${kind}` : ""}`;
      item.node.classList.toggle("attention", Boolean(kind));
      item.node.classList.toggle("critical", kind === "critical");
      continue;
    }
    if (!item.spec.test) continue;
    const count = s.items.filter(item.spec.test).length;
    setText(item.badge, count ? String(count) : "");
    item.badge.className = `badge${count && item.spec.badge ? ` ${item.spec.badge}` : ""}`;
    item.node.classList.toggle("attention", count > 0 && Boolean(item.spec.badge));
    item.node.classList.toggle("critical", count > 0 && item.spec.badge === "critical");
  }
}

// The download folder, above Hosts and Settings: opened with one click, its
// path shown on hover.
let folderNav = null;

function renderFolderNav(s) {
  if (!folderNav) {
    folderNav = el(
      "button",
      { type: "button", class: "nav-item", onclick: () => openPlace("downloads") },
      icon("folder-open"),
      el("span", { class: "label" }, "Download folder"),
    );
    $("nav-footer").prepend(folderNav);
  }
  const title = `Open ${s.download_dir}`;
  if (folderNav.title !== title) folderNav.title = title;
}

let commandsKey = null;
let commandButtons = null;

function makeCommands() {
  const search = el("input", { type: "search", id: "search", placeholder: "Search", "aria-label": "Search downloads" });
  search.addEventListener("input", () => {
    query = search.value.trim().toLowerCase();
    if (state) renderList(state);
  });
  commandButtons = el("span", { class: "commands" });
  $("commands").append(commandButtons, el("span", { class: "search" }, search, icon("search")));
}

function renderHead(s) {
  const spec = LISTS[view] ?? SECTIONS[view] ?? PAGES[view];
  setText($("title"), view === "all" ? "Downloads" : spec.label);
  $("commands").hidden = !(view in LISTS);
  const failed = s.items.filter((i) => i.status === "error");
  const done = s.items.some((i) => i.status === "done");
  const picked = s.items.filter((i) => selected.has(i.id));
  // What the selection's Remove asks depends on partial files and the bin setting too.
  const pickedKey = picked.map((i) => `${i.id}:${i.status}:${i.done > 0}`).join(",");
  // Downloads in this view that wait for the user: never started, or paused.
  const inView = LISTS[view]?.test ?? (() => false);
  const waiting = s.items.filter((i) => i.status === "paused" && inView(i));
  const fresh = waiting.length > 0 && waiting.every(notStarted);
  const key = `${s.paused}|${failed.length}|${done}|${view}|${s.recycle_on_remove}|${pickedKey}|${waiting.length}|${fresh}`;
  if (key === commandsKey) return;
  commandsKey = key;
  if (!commandButtons) makeCommands();
  // Only the buttons are rebuilt, so typing in the search box keeps focus.
  if (picked.length) {
    commandButtons.replaceChildren(...selectionCommands(picked, s).filter(Boolean));
    return;
  }
  // In the Failed view the broom clears failed downloads, partial files and
  // all, so it asks for a second click; elsewhere it tidies finished ones.
  const clear =
    view === "failed"
      ? failed.length
        ? confirmButton("Clear failed", `Clear ${plural(failed.length, "download")}?`, () => api("/api/global/clear_failed"), { icon: "broom", kind: "subtle" })
        : button("Clear failed", { icon: "broom", kind: "subtle", disabled: true })
      : button("Clear completed", {
          icon: "broom",
          kind: "subtle",
          disabled: !done,
          onclick: () => api("/api/global/clear_done").catch(report),
        });
  commandButtons.replaceChildren(...[
    button("Add links", { icon: "add", kind: "accent", onclick: () => openAdd() }),
    // Starts what waits for the user; the pause switch below does not.
    waiting.length
      ? button(`${fresh ? "Start" : "Resume"} ${waiting.length}`, {
          icon: "play",
          kind: "subtle",
          onclick: () => bulk(waiting, "resume"),
        })
      : null,
    button(s.paused ? "Unpause" : "Pause all", {
      icon: s.paused ? "play" : "pause",
      kind: "subtle",
      onclick: () => api(`/api/global/${s.paused ? "resume" : "pause"}`).catch(report),
    }),
    button("Retry failed", {
      icon: "arrow-clockwise",
      kind: "subtle",
      disabled: !failed.length,
      onclick: () => api("/api/global/retry_failed").catch(report),
    }),
    clear,
  ].filter(Boolean));
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// ---- selection: rows picked in the current view, Windows style

const selected = new Set();
let anchor = null; // where a Shift+click range starts

function visibleIds() {
  const spec = LISTS[view];
  if (!spec || !state) return [];
  const visible = state.items.filter((i) => spec.test(i) && matchesQuery(i));
  // Display order, with an archive's parts together even while folded.
  return entriesOf(visible, state.items).flatMap((e) => (e.members ? e.members.map((i) => i.id) : [e.it.id]));
}

function pick(id, options) {
  pickMany([id], options);
}

// Picks rows: alone (a plain click), added or dropped together (Ctrl), or a
// range from the last pick through them (Shift).
function pickMany(ids, { toggle = false, range = false } = {}) {
  const order = visibleIds();
  if (range && anchor && order.includes(anchor)) {
    const spots = [anchor, ...ids].map((id) => order.indexOf(id)).filter((i) => i >= 0);
    if (!toggle) selected.clear();
    for (const each of order.slice(Math.min(...spots), Math.max(...spots) + 1)) selected.add(each);
  } else if (toggle) {
    const all = ids.every((id) => selected.has(id));
    for (const id of ids) {
      if (all) selected.delete(id);
      else selected.add(id);
    }
    anchor = ids.at(-1);
  } else {
    selected.clear();
    for (const id of ids) selected.add(id);
    anchor = ids.at(-1);
  }
  if (state) render(state);
}

function clearSelection() {
  if (!selected.size) return;
  selected.clear();
  anchor = null;
  if (state) render(state);
}

// Space or Enter on a row's checkbox works like a Ctrl+click on the row, so
// the keyboard picks rows too.
function onCheckKeys(check, toggle) {
  check.addEventListener("keydown", (e) => {
    if (e.key !== " " && e.key !== "Enter") return;
    e.preventDefault();
    toggle();
  });
}

// Each action on every picked row it applies to; one report if any fail.
async function bulk(items, action) {
  const results = await Promise.allSettled(items.map((i) => api(`/api/item/${encodeURIComponent(i.id)}/${action}`)));
  const failure = results.find((r) => r.status === "rejected");
  if (failure) report(failure.reason);
  return results.filter((r) => r.status === "fulfilled").length;
}

function selectionCommands(picked, s) {
  const where = (test) => picked.filter(test);
  const retry = where((i) => i.status === "error");
  const pause = where((i) => ACTIVE.includes(i.status));
  const resume = where((i) => i.status === "paused");
  // Unfinished downloads lose their partial files, so that asks twice.
  const losing = where((i) => i.status !== "done" && i.done > 0).length;
  const recycling = s.recycle_on_remove ? where((i) => i.status === "done" && i.path && !i.existing).length : 0;
  const remove = async () => {
    const n = await bulk(picked, "remove");
    clearSelection();
    toast(recycling ? `Removed ${plural(n, "download")}, files to the ${s.bin}` : `Removed ${plural(n, "download")}`, "success");
  };
  const removeLabel = `Remove ${picked.length}`;
  const removeButton =
    losing || recycling
      ? confirmButton(removeLabel, losing ? `Delete ${plural(losing, "partial file")}?` : `Move ${plural(recycling, "file")} to the ${s.bin}?`, remove, {
          icon: "delete",
          kind: "subtle",
        })
      : button(removeLabel, { icon: "delete", kind: "subtle", onclick: () => remove().catch(report) });
  removeButton.dataset.role = "remove";
  return [
    el("span", { class: "picked", role: "status" }, `${picked.length} selected`),
    retry.length ? button("Retry", { icon: "arrow-clockwise", kind: "subtle", onclick: () => bulk(retry, "retry") }) : null,
    pause.length ? button("Pause", { icon: "pause", kind: "subtle", onclick: () => bulk(pause, "pause") }) : null,
    resume.length ? button(resume.every(notStarted) ? "Start" : "Resume", { icon: "play", kind: "subtle", onclick: () => bulk(resume, "resume") }) : null,
    removeButton,
    button("Clear selection", { icon: "dismiss", kind: "subtle", iconOnly: true, onclick: clearSelection }),
  ];
}

// Without a glyph the bar has no icon, like WinUI's IsIconVisible="False": inside a row, the row's glyph says it already.
function infobar(kind, glyph, title, message, actions = []) {
  return el(
    "div",
    { class: `infobar ${kind}${glyph ? "" : " plain"}`, role: kind === "critical" ? "alert" : undefined },
    glyph ? icon(glyph) : null,
    el("div", { class: "message" }, title ? el("b", {}, title) : null, message),
    actions.length ? el("div", { class: "bar-actions" }, actions) : null,
  );
}

let bannerKey = null;

// App-wide state only. A download that needs you has its own row and the Needs you badge.
function renderBanner(s) {
  // Paused matters only while it holds something back: a download that would
  // start, wait or retry. With nothing to hold, the status bar says it.
  const held = s.paused && s.items.some((i) => ACTIVE.includes(i.status));
  const key = `${offline}|${updated}|${held}|${s.config_error}`;
  if (key === bannerKey) return;
  bannerKey = key;
  const bars = [];
  if (offline) {
    bars.push(infobar("critical", "error-circle", "Lost the connection to Haul.", "Reconnecting…"));
  }
  if (updated) {
    bars.push(
      infobar("info", "info", "Haul was updated.", "Reload this page to use the new version.", [
        button("Reload", { icon: "arrow-clockwise", onclick: () => location.reload() }),
      ]),
    );
  }
  if (s.config_error) {
    bars.push(
      infobar("critical", "error-circle", "The settings file has an error.", "The previous settings stay in effect until it is fixed.", [
        button("Show", { onclick: () => go("settings") }),
      ]),
    );
  }
  if (held) {
    bars.push(
      infobar("info", "pause-circle", "Downloads are paused.", "Nothing new starts until you resume.", [
        button("Unpause", { icon: "play", onclick: () => api("/api/global/resume").catch(report) }),
      ]),
    );
  }
  $("banner").replaceChildren(...bars);
}

// ---- download rows

const rows = new Map();

const GLYPHS = {
  downloading: "arrow-circle-down",
  resolving: "clock",
  checking: "clock",
  queued: "clock",
  waiting: "clock",
  paused: "pause-circle",
  captcha: "warning",
  done: "checkmark-circle-filled",
  error: "error-circle",
};

// A file already in the download folder, being compared with the host's checksum.
function diskCaption(it) {
  if (state?.paused) return "Waiting to compare with the file on disk (paused)";
  return it.verifying === "comparing" ? "Comparing with the file on disk…" : "Waiting to compare with the file on disk";
}

function caption(it) {
  const of = it.size ? `${bytes(it.done)} of ${bytes(it.size)}` : it.done ? bytes(it.done) : "";
  const pct = it.size ? ` · ${Math.floor((it.done / it.size) * 100)}%` : "";
  switch (it.status) {
    case "downloading":
      return of ? `${of}${pct}` : "Downloading";
    case "queued":
      return it.size ? `Queued · ${bytes(it.size)}` : "Queued";
    case "checking":
      return it.verifying ? diskCaption(it) : "Checking the link…";
    case "resolving":
      return it.verifying ? diskCaption(it) : "Starting…";
    case "waiting":
      return it.wait ? `Retrying in ${duration(it.wait)}` : "Waiting";
    case "paused":
      if (!it.done) return it.size ? `Not started · ${bytes(it.size)}` : "Not started";
      return of ? `Paused · ${of}` : "Paused";
    case "captcha":
      return "Needs you";
    case "done":
      return `${it.existing ? "Already downloaded" : "Completed"}${it.size ? ` · ${bytes(it.size)}` : ""}`;
    case "error":
      return it.retry_in ? `Failed · retrying in ${duration(it.retry_in)}` : "Failed";
    default:
      return it.status;
  }
}

function rowActions(it, s) {
  const out = [];
  const subtle = (label, name, onclick) => button(label, { icon: name, kind: "subtle small", iconOnly: true, onclick });
  if (ACTIVE.includes(it.status)) out.push(subtle("Pause", "pause", () => act(it.id, "pause")));
  else if (it.status === "paused") out.push(subtle(notStarted(it) ? "Start" : "Resume", "play", () => act(it.id, "resume")));
  else if (it.status === "error") out.push(subtle("Retry", "arrow-clockwise", () => act(it.id, "retry")));
  if (it.status === "done" && it.path) {
    out.push(subtle("Show in folder", "folder-open", () => act(it.id, "reveal")));
  }
  if (it.status === "done" && it.path && s.recycle_on_remove) {
    const recycle = () =>
      api(`/api/item/${encodeURIComponent(it.id)}/remove`).then(() => toast(`Moved ${it.name} to the ${s.bin}`, "success"), report);
    out.push(subtle(`Move to the ${s.bin}`, "delete", recycle));
  } else if (it.status !== "done" && it.done > 0) {
    // Its partial file goes with it, so it asks twice, as Remove does for a selection.
    out.push(confirmButton("Remove", "Delete partial file?", () => act(it.id, "remove"), { icon: "dismiss", kind: "subtle small", iconOnly: true }));
  } else {
    out.push(subtle(it.status === "done" ? "Clear from list" : "Remove", "dismiss", () => act(it.id, "remove")));
  }
  return out;
}

function closeEditor(r) {
  r.editing = false;
  r.bar?.querySelector("form")?.remove();
}

function openEditor(r, it) {
  if (r.editing) {
    r.bar.querySelector("input")?.focus();
    return;
  }
  r.editing = true;
  const input = el("input", {
    type: "url",
    required: true,
    placeholder: "https://… the direct download link",
    "aria-label": "Direct download link",
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeEditor(r);
  });
  const form = el(
    "form",
    {
      onsubmit: async (e) => {
        e.preventDefault();
        try {
          await api(`/api/item/${encodeURIComponent(it.id)}/direct`, { url: input.value.trim() });
          closeEditor(r);
          toast(`Using your link for ${it.name}`, "success");
        } catch (error) {
          report(error);
        }
      },
    },
    input,
    el("button", { type: "submit", class: "button accent" }, "Use link"),
    button("Cancel", { onclick: () => closeEditor(r) }),
  );
  r.bar.append(form);
  input.focus();
}

function helpBar(r, it, s) {
  const actions = [];
  if (s.desktop) {
    actions.push(
      button("Open here", {
        icon: "globe",
        kind: "accent",
        onclick: () => openHere(it.id),
      }),
    );
  }
  for (const url of it.open_urls.filter(isWebLink)) {
    actions.push(
      el(
        "a",
        { class: `button${s.desktop ? "" : " accent"}`, href: url, target: "_blank", rel: "noopener noreferrer", title: url },
        icon("open"),
        s.desktop ? "Open in browser" : "Open page",
      ),
    );
  }
  actions.push(button("Paste link", { icon: "link", onclick: () => openEditor(r, it) }));
  actions.push(button("Try again", { icon: "arrow-clockwise", kind: "subtle", onclick: () => act(it.id, "retry_manual") }));
  const hint = s.desktop
    ? "Open here, solve the page and press its download button: Haul takes the download over."
    : "Open the page, solve it, then copy the final download link and paste it here.";
  const bar = infobar("caution", null, null, withoutHost(it.error, shownHost(it)), actions);
  bar.append(el("p", { class: "hint" }, hint));
  return bar;
}

function makeRow() {
  const r = { actionsKey: null, helpKey: null, glyphKey: null, editing: false, bar: null };
  r.glyph = el("span", { class: "glyph" });
  r.check = el("span", { class: "check", role: "checkbox", tabindex: "0", "aria-checked": "false", "aria-label": "Select" }, icon("checkmark"));
  onCheckKeys(r.check, () => pick(r.node.dataset.id, { toggle: true }));
  r.name = el("div", { class: "name" });
  r.sub = el("div", { class: "sub" });
  r.fill = el("i");
  r.caption = el("div", { class: "caption" });
  r.rate = el("div", { class: "rate" });
  r.actions = el("div", { class: "actions" });
  r.node = el(
    "li",
    { class: "row" },
    el(
      "div",
      { class: "row-main" },
      el("span", { class: "lead" }, r.glyph, r.check),
      el("div", { class: "title" }, r.name, r.sub),
      el("div", { class: "progress" }, el("div", { class: "bar" }, r.fill), r.caption),
      r.rate,
      r.actions,
    ),
  );
  // A click picks the row (Ctrl adds or drops it, Shift picks a range); the
  // row's own buttons, links and forms keep their clicks.
  r.node.addEventListener("mousedown", (e) => {
    if (e.shiftKey) e.preventDefault(); // no text selection on Shift+click
  });
  r.node.addEventListener("click", (e) => {
    if (e.target.closest("button, a, input, textarea, form")) return;
    const viaCheck = Boolean(e.target.closest(".check"));
    pick(r.node.dataset.id, { toggle: viaCheck || e.ctrlKey || e.metaKey, range: e.shiftKey });
  });
  return r;
}

function updateRow(r, it, s, member = false) {
  r.node.dataset.id = it.id;
  const picked = selected.has(it.id);
  r.check.setAttribute("aria-checked", String(picked));
  const rowClass = `row s-${it.status}${picked ? " selected" : ""}${member ? " member" : ""}`;
  if (r.node.className !== rowClass) r.node.className = rowClass;
  if (r.glyphKey !== it.status) {
    r.glyphKey = it.status;
    r.glyph.replaceChildren(icon(GLYPHS[it.status] ?? "clock"));
  }
  if (r.name.textContent !== it.name) {
    r.name.textContent = it.name;
    r.name.title = it.name;
    r.check.setAttribute("aria-label", `Select ${it.name}`);
  }

  // The host it comes from, starred when it is a preferred one, and how many
  // other hosts that are on could take over.
  const host = shownHost(it);
  const known = new Map((state?.hosts ?? []).map((h) => [h.name, h]));
  const sub = [`${known.get(host)?.preferred != null ? "★ " : ""}${host}`];
  const others = it.mirrors.filter((m) => m !== host && !known.get(m)?.disabled).length;
  if (others) sub.push(`${others} other ${others === 1 ? "host" : "hosts"}`);
  if (it.package) sub.push(it.package);
  if (wrapperNames.has(it.source)) sub.push(`from ${wrapperNames.get(it.source)}`);
  const error = ["error", "waiting"].includes(it.status) ? withoutHost(it.error, host) : "";
  const subKey = `${sub.join(" · ")}|${error}`;
  if (r.sub.dataset.key !== subKey) {
    r.sub.dataset.key = subKey;
    r.sub.replaceChildren(sub.join(" · "), error ? " · " : "", error ? el("span", { class: it.status === "error" ? "error" : "" }, error) : "");
    r.sub.title = [...sub, error].filter(Boolean).join("\n");
  }

  const pct = it.size ? Math.min(100, (it.done / it.size) * 100) : it.status === "done" ? 100 : (it.percent ?? 0);
  const width = `${pct.toFixed(1)}%`;
  if (r.fill.style.width !== width) r.fill.style.width = width;
  setText(r.caption, caption(it));

  const rateKey = it.speed ? `${bytes(it.speed)}/s|${it.size > it.done ? duration((it.size - it.done) / it.speed) : ""}` : "";
  if (r.rate.dataset.key !== rateKey) {
    r.rate.dataset.key = rateKey;
    const [speed, left] = rateKey.split("|");
    r.rate.replaceChildren(...(speed ? [el("b", {}, speed), left ? `${left} left` : ""] : []));
  }

  const actionsKey = `${it.status}|${Boolean(it.path)}|${s.recycle_on_remove}|${it.done > 0}`;
  if (r.actionsKey !== actionsKey) {
    r.actionsKey = actionsKey;
    r.actions.replaceChildren(...rowActions(it, s));
  }

  const needsHelp = it.status === "captcha";
  const helpKey = needsHelp ? `${it.error}|${it.open_urls.join(" ")}|${s.desktop}` : "";
  if (r.helpKey !== helpKey && !(r.editing && needsHelp)) {
    r.helpKey = helpKey;
    r.editing = false;
    r.bar?.remove();
    r.bar = needsHelp ? helpBar(r, it, s) : null;
    if (r.bar) r.node.append(r.bar);
  }
}

function matchesQuery(it) {
  if (!query) return true;
  return [it.name, it.package, it.host, wrapperNames.get(it.source), ...it.mirrors].some((v) => v?.toLowerCase().includes(query));
}

function rowFor(id) {
  let r = rows.get(id);
  if (!r) {
    r = makeRow();
    rows.set(id, r);
  }
  return r;
}

function renderList(s) {
  const list = $("list");
  const spec = LISTS[view];
  const visible = s.items.filter((i) => spec.test(i) && matchesQuery(i));
  const shownRows = new Set();
  const shownGroups = new Set();
  let previous = null;
  const place = (node) => {
    const want = previous ? previous.nextSibling : list.firstChild;
    if (want !== node) list.insertBefore(node, want);
    previous = node;
  };
  const show = (it, member) => {
    const r = rowFor(it.id);
    updateRow(r, it, s, member);
    shownRows.add(it.id);
    place(r.node);
  };
  for (const entry of entriesOf(visible, s.items)) {
    if (!entry.members) {
      show(entry.it, false);
      continue;
    }
    let g = groupRows.get(entry.key);
    if (!g) {
      g = makeGroupRow(entry.key);
      groupRows.set(entry.key, g);
    }
    updateGroupRow(g, entry);
    shownGroups.add(entry.key);
    place(g.node);
    if (isOpen(entry.key)) for (const it of entry.members) show(it, true);
  }
  for (const [id, r] of rows) {
    if (!shownRows.has(id)) {
      r.node.remove();
      rows.delete(id);
    }
  }
  for (const [key, g] of groupRows) {
    if (!shownGroups.has(key)) {
      g.node.remove();
      groupRows.delete(key);
    }
  }
  // Downloads that left the view (removed, finished elsewhere, filtered out)
  // drop out of the selection; folded parts stay in it.
  const inView = new Set(visible.map((i) => i.id));
  for (const id of selected) if (!inView.has(id)) selected.delete(id);
  list.classList.toggle("selecting", selected.size > 0);
  renderEmpty(s, visible.length);
}

// ---- archive groups: the parts of one multi-part archive under a row of
// their own, like JDownloader's packages

// The archive a file is a part of, and which part: name.part01.rar, name.rar
// with name.r00 and on, name.zip with name.z01 and on, or name.7z.001 (any
// name.NNN). Null for anything else.
function archivePart(name) {
  const forms = [
    [/^(.+)\.part0*(\d+)\.rar$/i, (m) => [`${m[1]}.rar`, Number(m[2])]],
    [/^(.+)\.r(\d{2})$/i, (m) => [`${m[1]}.rar`, Number(m[2]) + 2]],
    [/^(.+)\.rar$/i, (m) => [`${m[1]}.rar`, 1]],
    [/^(.+)\.z(\d{2})$/i, (m) => [`${m[1]}.zip`, Number(m[2])]],
    [/^(.+)\.zip$/i, (m) => [`${m[1]}.zip`, 100]],
    [/^(.+)\.(\d{3})$/, (m) => [m[1], Number(m[2])]],
  ];
  for (const [form, read] of forms) {
    const m = name?.match(form);
    if (m) {
      const [archive, part] = read(m);
      return { archive, part };
    }
  }
  return null;
}

// The list in display order: single rows, and each archive with two parts or
// more in the whole queue as one entry, at the place of its first part shown.
// An entry has every part of its archive (`parts`, which its row sums up in
// every view) and the parts this view shows (`members`), both in part order.
// Archives in different packages never mix.
function entriesOf(visible, items) {
  const keyOf = new Map();
  const sets = new Map();
  for (const it of items) {
    const p = archivePart(it.name);
    if (!p) continue;
    const key = `${it.package}\u0000${p.archive}`;
    keyOf.set(it.id, key);
    if (!sets.has(key)) sets.set(key, { key, archive: p.archive, parts: [] });
    sets.get(key).parts.push({ it, part: p.part });
  }
  const out = [];
  const placed = new Map();
  for (const it of visible) {
    const set = sets.get(keyOf.get(it.id));
    if (!set || set.parts.length < 2) {
      out.push({ it });
      continue;
    }
    let entry = placed.get(set.key);
    if (!entry) {
      set.parts.sort((a, b) => a.part - b.part);
      entry = { key: set.key, archive: set.archive, parts: set.parts.map((x) => x.it), members: [] };
      placed.set(set.key, entry);
      out.push(entry);
    }
    entry.members.push(it);
  }
  for (const entry of placed.values()) {
    const order = new Map(entry.parts.map((it, i) => [it.id, i]));
    entry.members.sort((a, b) => order.get(a.id) - order.get(b.id));
  }
  return out;
}

// Archives the user opened, in this browser only; folded is the default.
const expanded = new Set(
  (() => {
    try {
      return JSON.parse(prefs.get("expanded") ?? "[]");
    } catch {
      return [];
    }
  })(),
);

// A search opens every archive, so the parts it found show.
function isOpen(key) {
  return Boolean(query) || expanded.has(key);
}

function toggleGroup(key) {
  if (expanded.has(key)) expanded.delete(key);
  else expanded.add(key);
  prefs.set("expanded", JSON.stringify([...expanded].slice(-200)));
  if (state) render(state);
}

const groupRows = new Map();

// The glyph shows what needs attention first among the parts.
const URGENCY = ["error", "captcha", "downloading", "resolving", "waiting", "checking", "queued", "paused", "done"];

function makeGroupRow(key) {
  const g = { actionsKey: null, glyphKey: null, members: [] };
  g.glyph = el("span", { class: "glyph" });
  g.check = el("span", { class: "check", role: "checkbox", tabindex: "0", "aria-checked": "false", "aria-label": "Select its parts" }, icon("checkmark"), icon("subtract"));
  onCheckKeys(g.check, () => pickMany(g.members, { toggle: true }));
  g.toggle = button("Show the parts", { icon: "chevron-right", kind: "subtle small", iconOnly: true, onclick: () => toggleGroup(key) });
  g.toggle.setAttribute("aria-expanded", "false");
  g.name = el("div", { class: "name" });
  g.sub = el("div", { class: "sub" });
  g.fill = el("i");
  g.caption = el("div", { class: "caption" });
  g.rate = el("div", { class: "rate" });
  g.actions = el("div", { class: "actions" });
  g.node = el(
    "li",
    { class: "row group" },
    el(
      "div",
      { class: "row-main" },
      el("span", { class: "lead" }, g.glyph, g.check),
      el("div", { class: "title" }, el("div", { class: "group-name" }, g.toggle, g.name), g.sub),
      el("div", { class: "progress" }, el("div", { class: "bar" }, g.fill), g.caption),
      g.rate,
      g.actions,
    ),
  );
  // A click picks every part, with Ctrl and Shift as on single rows.
  g.node.addEventListener("mousedown", (e) => {
    if (e.shiftKey) e.preventDefault();
  });
  g.node.addEventListener("click", (e) => {
    if (e.target.closest("button, a, input")) return;
    const viaCheck = Boolean(e.target.closest(".check"));
    pickMany(g.members, { toggle: viaCheck || e.ctrlKey || e.metaKey, range: e.shiftKey });
  });
  return g;
}

// The row sums up the whole archive in every view; a click picks the parts
// this view shows.
function updateGroupRow(g, entry) {
  const parts = entry.parts;
  g.members = entry.members.map((i) => i.id);
  const picked = entry.members.filter((i) => selected.has(i.id)).length;
  const all = picked === entry.members.length;
  g.check.setAttribute("aria-checked", all ? "true" : picked ? "mixed" : "false");
  const status = URGENCY.find((st) => parts.some((i) => i.status === st)) ?? "queued";
  const complete = parts.every((i) => i.status === "done");
  const rowClass = `row group s-${status}${complete ? " complete" : ""}${all ? " selected" : ""}`;
  if (g.node.className !== rowClass) g.node.className = rowClass;
  if (g.glyphKey !== status) {
    g.glyphKey = status;
    g.glyph.replaceChildren(icon(GLYPHS[status] ?? "clock"));
  }
  if (g.name.textContent !== entry.archive) {
    g.name.textContent = entry.archive;
    g.name.title = entry.archive;
    g.check.setAttribute("aria-label", `Select the parts of ${entry.archive}`);
  }

  const size = parts.reduce((n, i) => n + i.size, 0);
  const done = parts.reduce((n, i) => n + (i.status === "done" ? i.size : Math.min(i.done, i.size || i.done)), 0);
  const hosts = [...new Set(parts.map((i) => shownHost(i)))];
  const from = wrapperNames.has(parts[0].source) ? `from ${wrapperNames.get(parts[0].source)}` : null;
  setText(g.sub, [`${parts.length} parts`, size ? bytes(size) : null, ...hosts, parts[0].package || null, from].filter(Boolean).join(" · "));

  // By bytes when every unfinished part's size is known, by parts otherwise.
  const sized = parts.every((i) => i.size > 0 || i.status === "done");
  const finished = parts.filter((i) => i.status === "done").length;
  const share = sized && size ? done / size : finished / parts.length;
  const width = `${Math.min(100, share * 100).toFixed(1)}%`;
  if (g.fill.style.width !== width) g.fill.style.width = width;
  setText(g.caption, partsSummary(parts));
  g.caption.title = size ? `${bytes(done)} of ${bytes(size)}` : "";

  const speed = parts.reduce((n, i) => n + (i.speed || 0), 0);
  const rateKey = speed ? `${bytes(speed)}/s|${size > done ? duration((size - done) / speed) : ""}` : "";
  if (g.rate.dataset.key !== rateKey) {
    g.rate.dataset.key = rateKey;
    const [shown, left] = rateKey.split("|");
    g.rate.replaceChildren(...(shown ? [el("b", {}, shown), left ? `${left} left` : ""] : []));
  }

  const open = isOpen(entry.key);
  g.toggle.hidden = Boolean(query);
  if (g.toggle.getAttribute("aria-expanded") !== String(open)) {
    const label = open ? "Hide the parts" : "Show the parts";
    g.toggle.setAttribute("aria-expanded", String(open));
    g.toggle.title = label;
    g.toggle.setAttribute("aria-label", label);
    g.toggle.replaceChildren(icon(open ? "chevron-down" : "chevron-right"));
  }

  const actionsKey = parts.map((i) => i.status).join(",");
  if (g.actionsKey !== actionsKey) {
    g.actionsKey = actionsKey;
    g.actions.replaceChildren(...groupActions(parts));
  }
}

// "12 of 18 done · 3 downloading · 3 failed"
function partsSummary(parts) {
  const count = (test) => parts.filter(test).length;
  const out = [`${count((i) => i.status === "done")} of ${parts.length} done`];
  for (const [n, label] of [
    [count((i) => i.status === "downloading"), "downloading"],
    [count((i) => ["queued", "waiting", "checking", "resolving"].includes(i.status)), "waiting"],
    [count(notStarted), "not started"],
    [count((i) => i.status === "paused" && !notStarted(i)), "paused"],
    [count((i) => i.status === "captcha"), "need you"],
    [count((i) => i.status === "error"), "failed"],
  ]) {
    if (n) out.push(`${n} ${label}`);
  }
  return out.join(" · ");
}

// Pause, resume or retry the parts together. Removing them goes through the
// selection: a click on the group picks them all.
function groupActions(parts) {
  const subtle = (label, name, onclick) => button(label, { icon: name, kind: "subtle small", iconOnly: true, onclick });
  const active = parts.filter((i) => ACTIVE.includes(i.status));
  const paused = parts.filter((i) => i.status === "paused");
  const failed = parts.filter((i) => i.status === "error");
  return [
    active.length ? subtle("Pause the parts", "pause", () => bulk(active, "pause")) : null,
    !active.length && paused.length
      ? subtle(paused.every(notStarted) ? "Start the parts" : "Resume the parts", "play", () => bulk(paused, "resume"))
      : null,
    failed.length ? subtle("Retry the failed parts", "arrow-clockwise", () => bulk(failed, "retry")) : null,
  ].filter(Boolean);
}

let emptyKey = null;

function renderEmpty(s, shown) {
  const box = $("empty");
  box.hidden = shown > 0;
  const key = shown ? "" : `${view}|${s.items.length > 0}|${query}`;
  if (shown || key === emptyKey) return;
  emptyKey = key;
  // With no downloads at all, the empty list says how to add some. It is no
  // box to drop into, since the whole window takes drops.
  const start = !query && (!s.items.length || view === "all");
  box.classList.toggle("start", start);
  if (query) {
    box.replaceChildren(icon("search"), el("strong", {}, "No matches"), `Nothing here matches “${query}”.`);
  } else if (start) {
    box.replaceChildren(
      icon("arrow-download"),
      el("strong", {}, "Drop links or files anywhere"),
      "Or paste links with Ctrl+V. Files can be .dlc containers or text files with links in them.",
    );
  } else {
    const [title, text] = EMPTY[view];
    box.replaceChildren(icon(LISTS[view].icon), el("strong", {}, title), text);
  }
}

// ---- wrappers

const wrapperCards = new Map();
// Each wrapper's name by its id, for the rows of the files it added.
let wrapperNames = new Map();

const wrapperName = (w) => w.title || w.url;

function startWrapper(w) {
  return api(`/api/wrapper/${encodeURIComponent(w.id)}/start`).then((r) => {
    if (r.started) toast(`Started ${plural(r.started, "file")} from ${wrapperName(w)}`, "success");
  }, report);
}

function removeWrapper(w) {
  return api(`/api/wrapper/${encodeURIComponent(w.id)}/remove`).catch(report);
}

// What a wrapper is doing, in a few words; once unwrapped, how its files stand.
function wrapperStatus(w, files) {
  switch (w.status) {
    case "unwrapping":
      return "Unwrapping…";
    case "waiting":
      return w.wait ? `Trying again in ${duration(w.wait)}` : "Waiting to try again";
    case "captcha":
      return "Needs you";
    case "paused":
      return "Paused";
    case "error":
      return w.retry_in ? `Failed · trying again in ${duration(w.retry_in)}` : "Failed";
    default: {
      const found = w.found ? `${plural(w.found.files, "file")} from ${plural(w.found.links, "link")}` : "Unwrapped";
      return files.length ? `${found} · ${partsSummary(files)}` : found;
    }
  }
}

// A gate a person passes: in the app's browser, which hands back the .dlc
// the page offers, or in their own, and a pasted link.
function wrapperHelp(c, w, s) {
  const it = { id: w.id, name: wrapperName(w), open_urls: w.open_urls, error: w.error, host: w.host, mirrors: [w.host] };
  const actions = [];
  if (s.desktop) actions.push(button("Open here", { icon: "globe", kind: "accent", onclick: () => openHere(w.id) }));
  for (const url of w.open_urls.filter(isWebLink)) {
    actions.push(
      el(
        "a",
        { class: `button${s.desktop ? "" : " accent"}`, href: url, target: "_blank", rel: "noopener noreferrer", title: url },
        icon("open"),
        s.desktop ? "Open in browser" : "Open page",
      ),
    );
  }
  actions.push(button("Paste link", { icon: "link", onclick: () => openEditor(c, it) }));
  actions.push(button("Try again", { icon: "arrow-clockwise", kind: "subtle", onclick: () => act(w.id, "retry_manual") }));
  const hint = s.desktop
    ? "Open here, pass the page's check and press its .dlc button: Haul unwraps what the page hands over."
    : "Open the page, pass its check, then copy the .dlc link it offers and paste it here.";
  const bar = infobar("caution", null, null, withoutHost(w.error, w.host), actions);
  bar.append(el("p", { class: "hint" }, hint));
  return bar;
}

function makeWrapperCard(id) {
  const c = { key: null, editing: false, bar: null };
  c.name = el("div", { class: "name" });
  c.sub = el("div", { class: "sub" });
  c.actions = el("span", { class: "host-actions" });
  c.node = el("div", { class: "card with-bar", "data-wrapper": id }, icon("link"), el("div", { class: "text" }, c.name, c.sub), c.actions);
  return c;
}

function renderWrappers(s) {
  const box = $("wrappers");
  let previous = null;
  const seen = new Set();
  for (const w of s.wrappers) {
    seen.add(w.id);
    let c = wrapperCards.get(w.id);
    if (!c) {
      c = makeWrapperCard(w.id);
      wrapperCards.set(w.id, c);
    }
    const files = s.items.filter((i) => i.source === w.id);
    const waiting = files.filter((i) => i.status === "paused");
    const unstarted = files.filter(notStarted);
    setText(c.name, wrapperName(w));
    c.name.title = w.url;
    const error = ["error", "waiting"].includes(w.status) ? withoutHost(w.error, w.host) : "";
    const subKey = `${w.host}|${wrapperStatus(w, files)}|${error}`;
    if (c.sub.dataset.key !== subKey) {
      c.sub.dataset.key = subKey;
      c.sub.replaceChildren(`${w.host} · ${wrapperStatus(w, files)}`, error ? " · " : "", error ? el("span", { class: w.status === "error" ? "error" : "" }, error) : "");
    }
    const key = `${w.status}|${waiting.length}|${unstarted.length}|${s.desktop}|${w.error}|${w.open_urls.join(" ")}`;
    if (c.key !== key && !(c.editing && w.status === "captcha")) {
      c.key = key;
      const actions = [];
      if (waiting.length) actions.push(button("Start all", { icon: "play", kind: "accent", onclick: () => startWrapper(w) }));
      if (w.status === "error") actions.push(button("Try again", { icon: "arrow-clockwise", kind: "subtle", onclick: () => act(w.id, "retry") }));
      // Its files that never started go with it, so that asks twice.
      actions.push(
        unstarted.length
          ? confirmButton("Remove", `Remove ${plural(unstarted.length, "file")} too?`, () => removeWrapper(w), { icon: "dismiss", kind: "subtle" })
          : button("Remove", { icon: "dismiss", kind: "subtle", onclick: () => removeWrapper(w) }),
      );
      c.actions.replaceChildren(...actions);
      c.editing = false;
      c.bar?.remove();
      c.bar = w.status === "captcha" ? wrapperHelp(c, w, s) : null;
      if (c.bar) c.node.append(c.bar);
    }
    const want = previous ? previous.node.nextSibling : box.firstChild;
    if (want !== c.node) box.insertBefore(c.node, want);
    previous = c;
  }
  for (const [id, c] of wrapperCards) {
    if (!seen.has(id)) {
      c.node.remove();
      wrapperCards.delete(id);
    }
  }
  const empty = $("no-wrappers");
  empty.hidden = s.wrappers.length > 0;
  if (!s.wrappers.length && !empty.childElementCount) {
    empty.append(icon("link"), el("strong", {}, "No wrapper links yet"), "Add a fileguard.cc or filecrypt.cc link: its files join the list, not started.");
  }
}

// ---- hosts

const hostCards = new Map();

function setRoute(name, route) {
  return api(`/api/host/${encodeURIComponent(name)}/route/${route}`);
}

function setPreferred(name, preferred) {
  api(`/api/host/${encodeURIComponent(name)}/preferred`, { preferred }).catch(report);
}

// Off: its links are never tried; on again, files that waited for it start.
function setDisabled(name, disabled) {
  api(`/api/host/${encodeURIComponent(name)}/disabled`, { disabled }).catch(report);
}

// The preferred hosts in the order downloads try them.
function preferredOrder(s) {
  return s.hosts
    .filter((h) => h.preferred !== null)
    .sort((a, b) => a.preferred - b.preferred)
    .map((h) => h.name);
}

function setPreferredOrder(hosts) {
  api("/api/preferred-hosts", { hosts }).catch(report);
}

// One place earlier (-1) or later (1) among the preferred hosts.
function movePreferred(name, delta) {
  if (!state) return;
  const order = preferredOrder(state);
  const from = order.indexOf(name);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= order.length) return;
  order.splice(to, 0, ...order.splice(from, 1));
  setPreferredOrder(order);
}

// Dragging a preferred host by its grip puts it elsewhere in the order. The
// drag carries a type of its own, so the window's drop zone for links and
// files leaves it alone, and the view keeps still until it ends.
const HOST_DRAG = "application/x-haul-host";
let dragged = null;
let dropAt = null;

function markDrop(target) {
  if (dropAt?.c === target?.c && dropAt?.after === target?.after) return;
  dropAt?.c.node.classList.remove("drop-before", "drop-after");
  dropAt = target;
  target?.c.node.classList.add(target.after ? "drop-after" : "drop-before");
}

function startHostDrag(e, c) {
  dragged = c;
  e.dataTransfer.effectAllowed = "move";
  e.dataTransfer.setData(HOST_DRAG, c.name);
  c.node.classList.add("dragging");
}

function endHostDrag(c) {
  c.node.draggable = false;
  c.node.classList.remove("dragging");
  markDrop(null);
  dragged = null;
  // Catch up with what came in during the drag.
  if (state) render(state);
}

$("hosts").addEventListener("dragover", (e) => {
  if (!dragged || !e.dataTransfer.types.includes(HOST_DRAG)) return;
  const node = e.target.closest?.(".card");
  const c = node ? hostCards.get(node.dataset.host) : null;
  if (!c?.movable || c === dragged) {
    markDrop(null);
    return;
  }
  e.preventDefault();
  e.dataTransfer.dropEffect = "move";
  const box = c.node.getBoundingClientRect();
  markDrop({ c, after: e.clientY > box.top + box.height / 2 });
});
$("hosts").addEventListener("dragleave", (e) => {
  if (!$("hosts").contains(e.relatedTarget)) markDrop(null);
});
$("hosts").addEventListener("drop", (e) => {
  if (!dragged || !dropAt || !state) return;
  e.preventDefault();
  e.stopPropagation();
  const order = preferredOrder(state).filter((name) => name !== dragged.name);
  order.splice(order.indexOf(dropAt.c.name) + (dropAt.after ? 1 : 0), 0, dragged.name);
  markDrop(null);
  setPreferredOrder(order);
});

// Downloads a host runs at once: your limit, or fewer when the host showed it
// takes fewer.
function slotsOf(host) {
  return host.cap ? Math.min(host.cap, host.limit) : host.limit;
}

function setLimit(name, delta) {
  const host = state?.hosts.find((h) => h.name === name);
  if (host) api(`/api/host/${encodeURIComponent(name)}/limit/${Math.max(1, slotsOf(host) + delta)}`).catch(report);
}

// A button that wants a second click within a few seconds. An icon-only one
// spells out its question while it waits.
function confirmButton(label, confirmLabel, action, options = {}) {
  const node = button(label, options);
  const plain = node.className;
  const text = options.iconOnly ? el("span", { class: "command-label" }) : node.lastChild;
  let timer = null;
  const show = (asking) => {
    node.className = asking ? `${plain.replace(" icon-only", "")} danger` : plain;
    text.textContent = asking ? confirmLabel : label;
    if (!options.iconOnly) return;
    if (asking) node.append(text);
    else text.remove();
    node.setAttribute("aria-label", asking ? confirmLabel : label);
  };
  node.addEventListener("click", () => {
    if (!timer) {
      show(true);
      timer = setTimeout(() => {
        timer = null;
        show(false);
      }, 4000);
      return;
    }
    clearTimeout(timer);
    timer = null;
    show(false);
    action().catch(report);
  });
  return node;
}

// The subfolder for one host's files, typed in a form under its card.
function editFolder(c, name) {
  if (c.node.querySelector("form")) {
    c.node.querySelector("input").focus();
    return;
  }
  const current = state?.hosts.find((h) => h.name === name)?.folder ?? "";
  const input = el("input", { type: "text", value: current, placeholder: "Subfolder", "aria-label": `Folder for files from ${name}` });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") form.remove();
  });
  const form = el(
    "form",
    {
      class: "inline-form",
      onsubmit: async (e) => {
        e.preventDefault();
        try {
          await api(`/api/host/${encodeURIComponent(name)}/folder`, { folder: input.value.trim() });
          form.remove();
        } catch (error) {
          report(error);
        }
      },
    },
    input,
    el("button", { type: "submit", class: "button accent" }, "Save"),
    button("Cancel", { onclick: () => form.remove() }),
    el("p", { class: "hint" }, `Files from ${name} go to this folder inside ${state?.download_dir ?? "the download folder"}. Leave it empty for the download folder itself.`),
  );
  c.node.append(form);
  input.focus();
}

// The most slots a host can have, as the engine allows (FALLBACK_LIMIT).
const MAX_SLOTS = 32;

function makeHostCard(name) {
  const c = { name, key: null, preferred: false, movable: false, disabled: false, showSlots: false };
  c.power = el("button", { type: "button", role: "switch", class: "toggle", onclick: () => setDisabled(name, !c.disabled) }, el("span", { class: "toggle-text" }), el("span", { class: "track" }, el("span", { class: "knob" })));
  c.limitButton = button("Limit downloads at once", {
    icon: "top-speed",
    kind: "subtle small",
    iconOnly: true,
    onclick: () => {
      c.showSlots = true;
      c.key = null;
      if (state) renderHosts(state);
    },
  });
  c.star = button("Prefer this host", { icon: "star", kind: "subtle small", iconOnly: true, onclick: () => setPreferred(name, !c.preferred) });
  c.up = button("Move up in the preferred order", { icon: "arrow-up", kind: "subtle small move", iconOnly: true, onclick: () => movePreferred(name, -1) });
  c.down = button("Move down in the preferred order", { icon: "arrow-down", kind: "subtle small move", iconOnly: true, onclick: () => movePreferred(name, 1) });
  c.order = el("span", { class: "order" }, c.up, c.down);
  // The host's icon, or the grip that drags it when it can move.
  c.mark = el("span", { class: "card-icon" }, icon("server"));
  c.mark.addEventListener("pointerdown", () => {
    if (c.movable) c.node.draggable = true;
  });
  c.stats = el("div", { class: "sub" });
  c.pills = el("span", { class: "controls" });
  c.slots = el("output");
  c.fewer = button("Fewer slots", { icon: "subtract", kind: "subtle small", iconOnly: true, onclick: () => setLimit(name, -1) });
  c.more = button("More slots", { icon: "add", kind: "subtle small", iconOnly: true, onclick: () => setLimit(name, 1) });
  c.node = el(
    "div",
    { class: "card with-bar host", "data-host": name },
    c.mark,
    el("div", { class: "text" }, el("div", {}, name), c.stats),
    c.pills,
    el(
      "span",
      { class: "host-actions" },
      (c.stepper = el("span", { class: "stepper", title: "Files downloaded from this host at the same time" }, c.fewer, c.slots, c.more)),
      c.limitButton,
      c.order,
      c.star,
      button("Folder for this host's files", { icon: "folder", kind: "subtle small", iconOnly: true, onclick: () => editFolder(c, name) }),
      c.power,
    ),
  );
  // Draggable only from the grip, so text in the card stays selectable; a
  // selection dragged out of the card is not the card.
  c.node.addEventListener("pointerup", () => (c.node.draggable = false));
  c.node.addEventListener("dragstart", (e) => {
    if (e.target === c.node) startHostDrag(e, c);
  });
  c.node.addEventListener("dragend", (e) => {
    if (e.target === c.node) endHostDrag(c);
  });
  return c;
}

function routeBars(h, s) {
  if (h.blocked && h.route === "proxy") {
    return [
      infobar("critical", "shield", "Looks blocked through the proxy.", "The host answered 403/451 through it.", [
        confirmButton("Allow direct", "Use my real IP address?", () => setRoute(h.name, "direct")),
      ]),
    ];
  }
  if (s.proxy && h.route === "direct") {
    return [
      infobar("caution", "shield", "Direct.", "This host sees your real IP address.", [
        button("Back to the proxy", { onclick: () => setRoute(h.name, "proxy").catch(report) }),
      ]),
    ];
  }
  return [];
}

const hostHeadings = new Map();

function hostHeading(kind) {
  let heading = hostHeadings.get(kind);
  if (!heading) {
    const [title, sub] = {
      preferred: ["Preferred, in order", "A file downloads from the first of these that has it."],
      other: ["Other hosts", "Used when no preferred host has the file, the fastest first."],
      off: ["Off", "Never used: their links are left alone."],
    }[kind];
    heading = el("div", { class: "host-group" }, el("strong", {}, title), el("span", { class: "sub" }, sub));
    hostHeadings.set(kind, heading);
  }
  return heading;
}

function ordinal(n) {
  return n === 1 ? "1st" : n === 2 ? "2nd" : n === 3 ? "3rd" : `${n}th`;
}

function renderHosts(s) {
  // A card being dragged stays put; the drag's end renders again.
  if (dragged) return;
  const box = $("hosts");
  // Moving a card takes the focus off its buttons; it goes back below.
  const focused = box.contains(document.activeElement) ? document.activeElement : null;
  let previous = null;
  const seen = new Set();
  const fastest = s.hosts.reduce((a, h) => (h.speed > (a?.speed ?? 0) ? h : a), null)?.name;
  const order = preferredOrder(s);
  const preferredCount = order.length;
  let group = null;
  s.hosts.forEach((h, index) => {
    seen.add(h.name);
    // A heading where the next group starts: preferred, the rest, off.
    const now = h.disabled ? "off" : h.preferred !== null ? "preferred" : "other";
    if (now !== group) {
      group = now;
      const heading = hostHeading(now);
      const want = previous ? previous.node.nextSibling : box.firstChild;
      if (want !== heading) box.insertBefore(heading, want);
      previous = { node: heading };
    }
    let c = hostCards.get(h.name);
    if (!c) {
      c = makeHostCard(h.name);
      hostCards.set(h.name, c);
    }
    const stats = [h.speed ? `${bytes(h.speed)}/s per file` : "Speed not measured yet", `${h.ok} done`, `${h.fails} failed`, `${bytes(h.bytes)} total`];
    if (h.cooldown) stats.push(`cooling down ${duration(h.cooldown)}`);
    // Its own word on what an address may fetch at full speed today.
    if (h.quota) stats.push(`${bytes(h.quota.used)} of ${bytes(h.quota.limit)} a day at full speed used`);
    setText(c.stats, stats.join(" · "));
    const slots = slotsOf(h);
    setText(c.slots, `${h.active} / ${slots} ${slots === 1 ? "slot" : "slots"}${h.exits > 1 ? ` × ${h.exits}` : ""}`);
    c.fewer.disabled = slots <= 1;
    c.more.disabled = slots >= MAX_SLOTS;
    // The slot buttons show when a limit is set or learned, or asked for.
    const limited = h.limit < MAX_SLOTS || h.cap !== null;
    c.stepper.hidden = !(limited || c.showSlots) || h.disabled;
    c.limitButton.hidden = !c.stepper.hidden || h.disabled;
    const best = h.name === fastest;
    const place = order.indexOf(h.name);
    const quota = h.quota ? `${h.quota.slowed}/${h.quota.known}` : "";
    const key = `${h.blocked}|${h.route}|${s.proxy}|${best}|${h.folder}|${h.preferred}|${preferredCount}|${h.cap}|${place}|${h.disabled}|${h.exits}|${quota}`;
    if (c.key !== key) {
      c.key = key;
      c.disabled = h.disabled;
      c.node.classList.toggle("off", h.disabled);
      c.power.setAttribute("aria-checked", String(!h.disabled));
      c.power.setAttribute("aria-label", h.disabled ? `Use ${h.name}` : `Never use ${h.name}`);
      c.power.title = h.disabled ? "Off: its links are never tried. Click to use it again." : "On: click to never use this host";
      c.power.querySelector(".toggle-text").textContent = h.disabled ? "Off" : "On";
      c.node.classList.toggle("best", best);
      // Two preferred hosts or more can trade places.
      c.movable = place >= 0 && preferredCount > 1;
      c.order.hidden = !c.movable;
      c.up.disabled = place === 0;
      c.down.disabled = place === preferredCount - 1;
      c.mark.classList.toggle("grip", c.movable);
      c.mark.replaceChildren(icon(c.movable ? "re-order-dots-vertical" : "server"));
      if (c.movable) c.mark.title = "Drag to change the order";
      else c.mark.removeAttribute("title");
      const pills = [];
      c.preferred = h.preferred !== null;
      const starTitle = c.preferred ? "Preferred: click to stop preferring it" : "Prefer this host";
      c.star.replaceChildren(icon(c.preferred ? "star-filled" : "star"));
      c.star.title = starTitle;
      c.star.setAttribute("aria-label", starTitle);
      c.star.setAttribute("aria-pressed", String(c.preferred));
      if (c.preferred) pills.push(el("span", { class: "pill preferred" }, icon("star-filled"), `${ordinal(h.preferred + 1)} choice`));
      if (h.disabled) pills.push(el("span", { class: "pill caution" }, "Never used"));
      if (h.exits > 1) pills.push(el("span", { class: "pill", title: "Downloads spread over these addresses; its limits count for each" }, icon("shield"), `${h.exits} addresses`));
      if (h.quota?.slowed) {
        const all = h.quota.slowed === h.quota.known;
        const title = "Past the bytes it allows an address at full speed today; it serves the rest slowly";
        pills.push(el("span", { class: `pill${all ? " caution" : ""}`, title }, icon("top-speed"), all ? `Slowed to ${bytes(h.quota.slowed_to)}/s` : `${h.quota.known - h.quota.slowed} of ${h.quota.known} addresses at full speed`));
      }
      if (best && !h.disabled) pills.push(el("span", { class: "pill success" }, "Fastest"));
      if (h.folder) pills.push(el("span", { class: "pill", title: "Its files go to this subfolder" }, icon("folder"), h.folder));
      if (h.cap && h.cap < h.limit) {
        const title = "It refused one more download while these ran, so Haul runs no more than this, and tries one more after ten quiet minutes. The slot buttons set your own limit.";
        pills.push(el("span", { class: "pill", title }, icon("info"), `Takes ${h.cap} at a time`));
      }
      if (s.proxy) pills.push(el("span", { class: `pill${h.route === "direct" ? " caution" : ""}` }, icon("shield"), `via ${h.route}`));
      c.pills.replaceChildren(...pills);
      for (const bar of c.node.querySelectorAll(":scope > .infobar")) bar.remove();
      for (const bar of routeBars(h, s)) c.node.append(bar);
    }
    const want = previous ? previous.node.nextSibling : box.firstChild;
    if (want !== c.node) box.insertBefore(c.node, want);
    previous = c;
  });
  for (const [kind, heading] of hostHeadings) {
    if (!s.hosts.some((h) => (h.disabled ? "off" : h.preferred !== null ? "preferred" : "other") === kind)) heading.remove();
  }
  for (const [name, c] of hostCards) {
    if (!seen.has(name)) {
      c.node.remove();
      hostCards.delete(name);
    }
  }
  // Back on the arrow it was on, or on the other one once that one ran out
  // (a disabled button loses the focus a moment later, so do not wait).
  if (focused?.isConnected && (focused.disabled || document.activeElement !== focused)) {
    const target = focused.disabled ? focused.parentElement.querySelector(".move:not(:disabled)") : focused;
    target?.focus({ preventScroll: true });
  }
  const empty = $("no-hosts");
  empty.hidden = s.hosts.length > 0;
  if (!s.hosts.length && !empty.childElementCount) {
    empty.append(icon("server"), el("strong", {}, "No host contacted yet"), "Hosts appear here once Haul talks to them.");
  }
}

// ---- settings

let settingsKey = null;
let settingsData = null;
let settingsVersion = null;

// Every setting, account secrets included: fetched for this page only, never
// part of the live state.
async function loadSettings() {
  try {
    const res = await fetch("/api/settings");
    if (res.ok) settingsData = await res.json();
  } catch (error) {
    report(error);
  }
  settingsKey = null;
  if (state && view === "settings") render(state);
}

async function save(changes, saved = "Saved") {
  try {
    await api("/api/settings", changes);
    if (saved) toast(saved, "success");
    return true;
  } catch (error) {
    report(error);
    settingsKey = null;
    if (state) render(state);
    return false;
  }
}

function settingCard(glyph, title, description, ...controls) {
  return el(
    "div",
    { class: "card" },
    icon(glyph),
    el("div", { class: "text" }, el("div", {}, title), description ? el("div", { class: "sub" }, description) : null),
    el("div", { class: "controls" }, controls),
  );
}

// Why a key cannot change here, when an environment variable sets it.
function envNote(...keys) {
  const variable = keys.map((k) => settingsData.from_env[k]).find(Boolean);
  return variable ? `Set by the ${variable} environment variable.` : null;
}

function numberField(key, value, { min, max, step = 1, label, unit }) {
  const input = el("input", { type: "number", min, max, step, value, "aria-label": label });
  input.addEventListener("change", () => {
    const n = Number(input.value);
    if (!Number.isFinite(n) || n < min || (max !== undefined && n > max)) {
      input.value = value;
      toast(max === undefined ? `${label}: ${min} or more` : `${label}: from ${min} to ${max}`, "error");
      return;
    }
    save({ [key]: n });
  });
  return unit ? el("span", { class: "with-unit" }, input, el("span", { class: "hint" }, unit)) : input;
}

function textField(key, value, { label, placeholder = "", secret = false, wide = false }) {
  const input = el("input", {
    type: secret ? "password" : "text",
    value,
    placeholder,
    "aria-label": label,
    autocomplete: "off",
    spellcheck: "false",
    disabled: Boolean(settingsData.from_env[key]),
    class: wide ? "wide" : undefined,
  });
  input.addEventListener("change", () => save({ [key]: input.value.trim() }));
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") input.blur();
  });
  return secret ? withReveal(input) : input;
}

// The extra proxies, one per line; saved when the box loses focus.
function proxyList(d) {
  const box = el("textarea", {
    rows: Math.min(8, Math.max(3, d.proxies.length + 1)),
    spellcheck: "false",
    autocomplete: "off",
    class: "wide mono",
    "aria-label": "More proxies, one per line",
    placeholder: "socks5://user:pass@amsterdam.example:1080\nsocks5://127.0.0.1:1081",
  });
  box.value = d.proxies.join("\n");
  box.addEventListener("change", () => save({ proxies: box.value }));
  return box;
}

// A password field with a button that shows what it holds.
function withReveal(input) {
  const reveal = button("Show", {
    icon: "eye",
    kind: "subtle small",
    iconOnly: true,
    onclick: () => {
      const hidden = input.type === "password";
      input.type = hidden ? "text" : "password";
      reveal.replaceChildren(icon(hidden ? "eye-off" : "eye"));
      reveal.title = hidden ? "Hide" : "Show";
    },
  });
  return el("span", { class: "secret" }, input, reveal);
}

const accountField = (key, label, secret = true) =>
  textField(key, settingsData.accounts[key] ?? "", { label, placeholder: label, secret });

function toggleSwitch(key, on, label) {
  return el(
    "button",
    { type: "button", role: "switch", class: "toggle", "aria-checked": String(on), "aria-label": label, onclick: () => save({ [key]: !on }, null) },
    el("span", { class: "toggle-text" }, on ? "On" : "Off"),
    el("span", { class: "track" }, el("span", { class: "knob" })),
  );
}

function openPlace(place) {
  return api(`/api/open/${place}`).catch(report);
}

async function changeFolder(card, s) {
  if (s.desktop) {
    try {
      const r = await api("/api/settings/download-dir/pick");
      if (!r.cancelled) toast("Download folder changed; unfinished downloads moved along", "success");
    } catch (error) {
      report(error);
    }
    return;
  }
  // A browser cannot show this machine's folders: type the path instead.
  if (card.querySelector("form")) return;
  const input = el("input", { type: "text", value: settingsData.download_dir, class: "wide", "aria-label": "Download folder" });
  const form = el(
    "form",
    {
      class: "inline-form",
      onsubmit: async (e) => {
        e.preventDefault();
        if (await save({ download_dir: input.value.trim() }, "Download folder changed")) form.remove();
      },
    },
    input,
    el("button", { type: "submit", class: "button accent" }, "Save"),
    button("Cancel", { onclick: () => form.remove() }),
  );
  card.append(form);
  input.focus();
}

function folderCard(s) {
  const d = settingsData;
  const description = el(
    "span",
    {},
    el("span", { class: "mono" }, d.download_dir),
    d.download_dir_fixed ? el("span", {}, el("br"), "Given on the command line with --dir.") : null,
  );
  const card = settingCard(
    "folder-open",
    "Download folder",
    description,
    // The status bar opens it, on this page as on every other.
    d.download_dir_fixed ? null : button("Change…", { onclick: () => changeFolder(card, s) }),
  );
  card.classList.add("with-bar");
  return card;
}

function renderSettings(s) {
  if (s.config_version !== settingsVersion) {
    settingsVersion = s.config_version;
    loadSettings();
  }
  const d = settingsData;
  if (!d) return;
  const key = JSON.stringify([d, s.desktop, theme, s.config_error, s.version]);
  // Leave the page alone while a field is being edited.
  const active = document.activeElement;
  if (key === settingsKey || ($("settings").contains(active) && active.matches("input"))) return;
  settingsKey = key;
  const themes = el(
    "div",
    { class: "segmented", role: "group", "aria-label": "App theme" },
    THEMES.map(([value, label, glyph]) =>
      el("button", { type: "button", "aria-pressed": String(theme === value), onclick: () => setTheme(value) }, icon(glyph), label),
    ),
  );
  fill(
    $("settings"),
    s.config_error
      ? infobar("critical", "error-circle", "The settings file has an error.", `The previous settings stay in effect until it is fixed. ${s.config_error}`, [
          button("Open the file", { onclick: () => openPlace("config") }),
        ])
      : null,
    el("h2", {}, "Appearance"),
    settingCard("dark-theme", "App theme", "Follow Windows, or pick one.", themes),
    el("h2", {}, "Downloads"),
    settingCard("arrow-download", "Parallel downloads", "Files downloaded at the same time.", numberField("max_total", d.max_total, { min: 1, max: 64, label: "Parallel downloads" })),
    settingCard(
      "document",
      "Size downloading at once",
      "The most gigabytes of files downloading at the same time, by their full size: 20 lets four 5 GB files run, or ten of 2 GB. A bigger file still downloads on its own. 0 means no limit.",
      numberField("max_active_gb", d.max_active_gb, { min: 0, step: 1, label: "Size downloading at once", unit: "GB" }),
    ),
    settingCard(
      "top-speed",
      "Speed limit",
      "Shared by every transfer. 0 means no limit.",
      numberField("rate_limit_mbps", d.rate_limit_mbps, { min: 0, step: 0.5, label: "Speed limit", unit: "MB/s" }),
    ),
    folderCard(s),
    settingCard(
      "delete",
      `Move removed files to the ${s.bin}`,
      "When you remove a finished download from the list, its file goes too. Clear completed only tidies the list.",
      toggleSwitch("recycle_on_remove", d.recycle_on_remove, `Move removed files to the ${s.bin}`),
    ),
    settingCard(
      "arrow-download",
      "Add dropped links and files right away",
      "Off: links and .dlc / .txt files you drop on the window, or open with Haul, fill the Add dialog first, so you can check them before anything downloads.",
      toggleSwitch("skip_add_dialog", d.skip_add_dialog, "Add dropped links and files right away"),
    ),
    settingCard(
      "checkmark-circle",
      "Compare checksums of files already there",
      "A file in the download folder with the same name and size counts as downloaded. With this on, Haul also reads it in full and compares it with the checksum its host publishes, one file at a time and never while downloads are paused: thorough, but slow on big files and spinning disks.",
      toggleSwitch("verify_existing", d.verify_existing, "Compare checksums of files already there"),
    ),
    s.desktop
      ? settingCard(
          "clipboard-link",
          "Watch the clipboard",
          "Links you copy anywhere are added, when they point at a supported host or a file.",
          toggleSwitch("watch_clipboard", d.watch_clipboard, "Watch the clipboard"),
        )
      : null,
    el("h2", {}, "When something fails"),
    settingCard(
      "arrow-clockwise",
      "Retries after network trouble",
      "Per mirror, after a dropped connection or an unreachable host; then the next mirror takes over.",
      numberField("resume_attempts", d.resume_attempts, { min: 0, max: 20, label: "Retries", unit: "times" }),
    ),
    settingCard(
      "top-speed",
      "Give up on slow downloads",
      `Below this speed for ${d.min_speed_seconds === 60 ? "a minute" : `${d.min_speed_seconds} seconds`}, the next mirror takes over, or the download fails until it is tried again. 0 turns it off; so does a speed limit.`,
      numberField("min_speed_kbps", d.min_speed_kbps, { min: 0, max: 1048576, label: "Minimum speed", unit: "KB/s" }),
    ),
    settingCard(
      "clock",
      "Retry failed downloads",
      "Tries a failed download again after this many minutes, up to three times. 0 turns it off.",
      numberField("retry_failed_minutes", d.retry_failed_minutes, { min: 0, max: 1440, label: "Minutes", unit: "min" }),
    ),
    el("h2", {}, "Network"),
    settingCard(
      "shield",
      "Proxy",
      envNote("proxy") ?? "Every host goes through it unless you allow it direct under Hosts. Leave empty to connect directly.",
      textField("proxy", d.proxy, { label: "Proxy", placeholder: "socks5://user:pass@host:1080", secret: true, wide: true }),
      d.proxy ? toggleSwitch("use_proxy", d.use_proxy, "Use the proxy") : null,
    ),
    settingCard(
      "shield",
      "More proxies",
      "One per line. Each download takes the proxy with the fewest downloads from its host, so a host's limits per address (downloads at once, daily quotas, waits) apply to each one. A VPN's SOCKS5 servers, local WireGuard-to-SOCKS5 ports or a bought list all fit.",
      proxyList(d),
    ),
    d.proxy || d.proxies.length
      ? settingCard("desktop", "My own connection too", "Adds this computer's connection to the proxies, as one more address. Hosts then see your real IP address.", toggleSwitch("proxy_with_direct", d.proxy_with_direct, "My own connection too"))
      : null,
    d.proxy || d.proxies.length
      ? settingCard(
          "arrow-clockwise",
          "New connection for every download",
          "For a proxy that gives each connection another address (a rotating endpoint), so each download gets its own.",
          toggleSwitch("proxy_fresh_connections", d.proxy_fresh_connections, "New connection for every download"),
        )
      : null,
    el("h2", {}, "Accounts"),
    settingCard("key", "1fichier", envNote("1fichier_key") ?? "API key: no free-mode wait or captcha.", accountField("1fichier_key", "API key")),
    settingCard("key", "pixeldrain", envNote("pixeldrain_key") ?? "API key.", accountField("pixeldrain_key", "API key")),
    settingCard("key", "gofile", envNote("gofile_token") ?? "Account token: guests cannot use the API for every file.", accountField("gofile_token", "Account token")),
    settingCard(
      "key",
      "rapidgator",
      envNote("rapidgator_user", "rapidgator_pass") ?? "Premium account.",
      accountField("rapidgator_user", "Username", false),
      accountField("rapidgator_pass", "Password"),
    ),
    settingCard(
      "key",
      "nitroflare",
      envNote("nitroflare_user", "nitroflare_key") ?? "Premium account.",
      accountField("nitroflare_user", "User", false),
      accountField("nitroflare_key", "Premium key"),
    ),
    el("h2", {}, "Advanced"),
    settingCard(
      "wrench",
      "megatools",
      "For mega.nz links. Leave empty to find it on PATH.",
      textField("megatools", d.megatools, { label: "megatools", placeholder: "C:\\Tools\\megatools\\megatools.exe", wide: true }),
    ),
    settingCard(
      "globe",
      "User agent",
      "Sent to hosts. Leave empty for Haul's default.",
      textField("user_agent", d.user_agent === d.default_user_agent ? "" : d.user_agent, { label: "User agent", placeholder: d.default_user_agent, wide: true }),
    ),
    el("h2", {}, "Files"),
    d.config_file
      ? settingCard(
          "document",
          "Settings file",
          el("span", {}, el("span", { class: "mono" }, d.config_file), el("br"), "Every setting on this page is stored here. Edits saved in a text editor apply within a second."),
          button("Open", { onclick: () => openPlace("config") }),
        )
      : null,
    settingCard("folder-open", "Data folder", el("span", { class: "mono" }, s.data_dir), button("Open folder", { onclick: () => openPlace("data") })),
    el("h2", {}, "About"),
    settingCard("info", `Haul ${s.version}`, s.desktop ? "Download manager for file hosts, desktop app." : "Download manager for file hosts, web UI."),
    settingCard(
      "wrench",
      "Diagnostics",
      "A report of what Haul is doing, to send along when something goes wrong: every download and wrapper with its links, state, error and last events, the hosts, the settings and the recent log. Account keys, proxy passwords and mega.nz keys are left out.",
      button("Save diagnostics…", { onclick: saveDiagnostics }),
    ),
    settingCard(
      "document",
      "Detailed log",
      "Writes every request, retry and mirror choice to the log, for reproducing a problem before you save the report. Off, the log keeps notable events. Each download keeps its last events either way.",
      toggleSwitch("detailed_log", d.detailed_log, "Detailed log"),
    ),
  );
}

// The desktop app asks where to save the report; a browser downloads it.
async function saveDiagnostics() {
  if (!state?.desktop) {
    const link = el("a", { href: "/api/diagnostics", download: "" });
    document.body.append(link);
    link.click();
    link.remove();
    return;
  }
  try {
    const r = await api("/api/diagnostics/save");
    if (r.saved) toast(`Saved the report to ${r.saved}`, "success");
  } catch (error) {
    report(error);
  }
}

// ---- status bar

let statusKey = null;

function renderStatus(s) {
  const downloading = s.items.filter((i) => i.status === "downloading").length;
  const queued = s.items.filter((i) => ["queued", "waiting", "checking", "resolving"].includes(i.status)).length;
  const clipboard = s.desktop && s.watch_clipboard;
  const key = JSON.stringify([bytes(s.speed), downloading, queued, s.rate, s.proxy, theme, s.paused, clipboard]);
  if (key === statusKey) return;
  statusKey = key;
  const themeSpec = THEMES.find(([value]) => value === theme) ?? THEMES[0];
  const next = THEMES[(THEMES.indexOf(themeSpec) + 1) % THEMES.length][0];
  // Items that change a setting open a flyout with it.
  const flyoutItem = (key, title, open, ...content) =>
    el("button", { type: "button", class: `status-item${(key === "proxy" && s.proxy) || key === "clipboard" ? " on" : ""}`, title, "aria-haspopup": "dialog", onclick: (e) => open(e.currentTarget) }, ...content);
  fill(
    $("status"),
    el("span", { class: `status-item${s.speed ? " on" : ""}` }, icon("arrow-download"), `${bytes(s.speed)}/s`),
    el("span", { class: "status-item" }, s.paused ? "Paused" : `${downloading} downloading · ${queued} waiting`),
    el("span", { class: "grow" }),
    clipboard ? flyoutItem("clipboard", "Links you copy are added", clipboardFlyout, icon("clipboard-link"), "Watching the clipboard") : null,
    flyoutItem("speed", "Speed limit", speedFlyout, icon("top-speed"), s.rate ? `${(s.rate / 1e6).toFixed(1)} MB/s cap` : "No speed limit"),
    flyoutItem("proxy", "Proxy", proxyFlyout, icon("shield"), s.proxy ? "Proxy on" : "Proxy off"),
    el(
      "button",
      { type: "button", class: "status-item", title: `Theme: ${themeSpec[1]} (click for ${next})`, onclick: () => setTheme(next) },
      icon(themeSpec[2]),
      themeSpec[1],
    ),
  );
}

// ---- flyouts: small panels over the status bar that change a setting in place

const flyout = el("div", { class: "flyout", popover: "auto", role: "dialog" });
document.body.append(flyout);
let flyoutFor = null;
let flyoutClosed = { key: null, at: 0 };
flyout.addEventListener("beforetoggle", (e) => {
  if (e.newState === "closed") flyoutClosed = { key: flyoutFor, at: performance.now() };
});

// Opens the flyout `key` above `anchor`, filled by `build` from the current
// settings. A click on its item while it is open closes it, as light dismiss
// does just before the click arrives.
async function openFlyout(key, anchor, title, build) {
  if (typeof flyout.showPopover !== "function") return go("settings");
  if (flyoutClosed.key === key && performance.now() - flyoutClosed.at < 400) return;
  const rect = anchor.getBoundingClientRect();
  let d;
  try {
    const res = await fetch("/api/settings");
    if (!res.ok) throw new Error(`Settings: HTTP ${res.status}`);
    d = await res.json();
  } catch (error) {
    return report(error);
  }
  flyoutFor = key;
  flyout.setAttribute("aria-label", title);
  fill(flyout, el("div", { class: "flyout-title" }, title), build(d));
  if (!flyout.matches(":popover-open")) flyout.showPopover();
  flyout.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - flyout.offsetWidth - 8))}px`;
  flyout.style.top = `${Math.max(8, rect.top - flyout.offsetHeight - 8)}px`;
  flyout.querySelector("input:not([disabled]), button:not([disabled])")?.focus();
}

function closeFlyout() {
  if (flyout.matches(":popover-open")) flyout.hidePopover();
}

const fromEnv = (d, key) => (d.from_env[key] ? `Set by the ${d.from_env[key]} environment variable.` : null);

// A switch that flips at once and saves; `change` answers whether it saved.
function flyoutSwitch(label, on, change, disabled = false) {
  const text = el("span", { class: "toggle-text" });
  const node = el("button", { type: "button", role: "switch", class: "toggle", "aria-label": label, disabled }, text, el("span", { class: "track" }, el("span", { class: "knob" })));
  const show = (value) => {
    node.setAttribute("aria-checked", String(value));
    setText(text, value ? "On" : "Off");
  };
  show(on);
  node.addEventListener("click", async () => {
    const next = node.getAttribute("aria-checked") !== "true";
    show(next);
    if (!(await change(next))) show(!next);
  });
  return el("div", { class: "flyout-row" }, el("span", { class: "grow" }, label), node);
}

function speedFlyout(anchor) {
  openFlyout("speed", anchor, "Speed limit", (d) => {
    const fixed = fromEnv(d, "rate_limit_mbps");
    const input = el("input", { type: "number", min: 0, step: 0.5, value: d.rate_limit_mbps || "", placeholder: "None", "aria-label": "Speed limit in MB/s", disabled: Boolean(fixed) });
    const apply = async (mbps) => {
      if (await save({ rate_limit_mbps: mbps }, null)) closeFlyout();
    };
    const set = () => {
      const n = input.value.trim() === "" ? 0 : Number(input.value);
      if (Number.isFinite(n) && n >= 0) apply(n);
      else input.focus();
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") set();
    });
    return [
      el("p", { class: "hint" }, fixed ?? "For all downloads together."),
      el("div", { class: "flyout-row" }, el("span", { class: "with-unit grow" }, input, el("span", { class: "hint" }, "MB/s")), button("Set", { kind: "accent", onclick: set, disabled: Boolean(fixed) })),
      d.rate_limit_mbps > 0 && !fixed ? button("Remove the limit", { kind: "subtle", onclick: () => apply(0) }) : null,
    ];
  });
}

function proxyFlyout(anchor) {
  openFlyout("proxy", anchor, "Proxy", (d) => {
    const fixed = fromEnv(d, "proxy");
    let use = flyoutSwitch("Use the proxy", Boolean(d.proxy) && d.use_proxy, (on) => save({ use_proxy: on }, null), !d.proxy);
    const address = el("input", {
      type: "password",
      value: d.proxy ?? "",
      placeholder: "socks5://user:pass@host:1080",
      "aria-label": "Proxy address",
      autocomplete: "off",
      spellcheck: "false",
      disabled: Boolean(fixed),
    });
    // A new address is meant to be used.
    address.addEventListener("change", async () => {
      const value = address.value.trim();
      if (!(await save(value ? { proxy: value, use_proxy: true } : { proxy: "" }, null))) return;
      const next = flyoutSwitch("Use the proxy", Boolean(value), (on) => save({ use_proxy: on }, null), !value);
      use.replaceWith(next);
      use = next;
    });
    address.addEventListener("keydown", (e) => {
      if (e.key === "Enter") address.blur();
    });
    return [use, withReveal(address), el("p", { class: "hint" }, fixed ?? "Every host goes through it unless you allow it direct under Hosts.")];
  });
}

function clipboardFlyout(anchor) {
  openFlyout("clipboard", anchor, "Clipboard", (d) => [
    el("p", { class: "hint" }, "Links to known hosts and files that you copy anywhere are added."),
    flyoutSwitch("Watch the clipboard", d.watch_clipboard, async (on) => {
      const saved = await save({ watch_clipboard: on }, null);
      if (saved && !on) closeFlyout();
      return saved;
    }),
  ]);
}

// ---- adding links

const dialog = $("add-dialog");

// Opens the Add dialog, or adds `text` to it when it is open already; `pkg`
// fills in the package unless one is typed in. Each time it opens it starts
// empty: a package left over from a dropped container would otherwise quietly
// put the next links in its folder.
function openAdd(text = "", pkg = "") {
  if (!dialog.open) {
    $("add-text").value = "";
    $("add-package").value = "";
    dialog.showModal();
  }
  if (text) {
    const current = $("add-text").value.trim();
    $("add-text").value = current ? `${current}\n${text}` : text;
  }
  if (pkg && !$("add-package").value.trim()) $("add-package").value = pkg;
  $("add-text").focus();
}

async function addText(text, pkg = "") {
  if (!text.trim()) return false;
  try {
    const r = await api("/api/add", { text, package: pkg });
    toast(addedMessage(r), r.added + r.merged ? "success" : "info");
    return r.added + r.merged + r.duplicates > 0;
  } catch (error) {
    report(error);
    return false;
  }
}

function filesForm(files) {
  const form = new FormData();
  for (const file of files) form.append("file", file, file.name);
  return form;
}

async function importFiles(files) {
  if (!files.length) return;
  try {
    toast(addedMessage(await api("/api/container", filesForm(files))), "success");
  } catch (error) {
    toast(`Import failed: ${error.message}`, "error");
  }
}

// The links in .dlc and text files go into the Add dialog; nothing is added
// until the user confirms there.
async function readFiles(files) {
  if (!files.length) return;
  try {
    const r = await api("/api/container/read", filesForm(files));
    if (!r.links.length) {
      toast("No links in that file", "info");
      return;
    }
    openAdd(r.links.join("\n"), r.package);
  } catch (error) {
    toast(`Import failed: ${error.message}`, "error");
  }
}

$("add-import").append(icon("arrow-upload"), "Import file…");
$("add-import").addEventListener("click", () => $("add-file").click());
$("add-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (await addText($("add-text").value, $("add-package").value.trim())) dialog.close();
});
$("add-cancel").addEventListener("click", () => dialog.close());
$("add-text").addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    $("add-form").requestSubmit();
  }
});
$("add-file").addEventListener("change", async (e) => {
  await readFiles(e.target.files);
  e.target.value = "";
});

// Paste links anywhere outside a text field. With one download waiting for a
// person, a single pasted link is the one its page just handed back: it goes
// to that download rather than starting a new one.
document.addEventListener("paste", (e) => {
  if (e.target.closest?.("input, textarea") || dialog.open) return;
  const text = e.clipboardData?.getData("text") ?? "";
  if (!/https?:\/\//i.test(text)) return;
  e.preventDefault();
  const link = text.trim();
  const waiting = state ? state.items.filter((i) => i.status === "captcha") : [];
  if (waiting.length === 1 && !/\s/.test(link)) {
    const it = waiting[0];
    api(`/api/item/${encodeURIComponent(it.id)}/direct`, { url: link }).then(
      () => toast(`Using your link for ${it.name}`, "success"),
      report,
    );
    return;
  }
  openAdd(text);
});

document.addEventListener("keydown", (e) => {
  const typing = e.target.closest?.("input, textarea");
  if (typing || dialog.open || !(view in LISTS)) return;
  if (e.key === "/") {
    e.preventDefault();
    $("search")?.focus();
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
    e.preventDefault();
    for (const id of visibleIds()) selected.add(id);
    if (state) render(state);
  } else if (e.key === "Escape" && selected.size) {
    clearSelection();
  } else if (e.key === "Delete" && selected.size) {
    // Same as the Remove button, second press included where it asks.
    commandButtons?.querySelector("[data-role='remove']")?.click();
  }
});

// A click on the list's empty space, outside any row, drops the selection.
$("list").parentElement.addEventListener("click", (e) => {
  if (!e.target.closest(".row, button, input, a, .infobar")) clearSelection();
});

// Drop links or container files anywhere.
let dragDepth = 0;
const carriesData = (e) => [...(e.dataTransfer?.types ?? [])].some((t) => t === "Files" || t.startsWith("text/"));
$("drop").firstElementChild.append(
  icon("arrow-download"),
  el("strong", {}, "Drop to add to Haul"),
  "Links, .dlc containers and text files with links in them.",
);
// The whole window lights up as the drop zone while something is dragged over.
addEventListener("dragenter", (e) => {
  if (!carriesData(e) || dialog.open) return;
  dragDepth += 1;
  $("drop").hidden = false;
});
function endDrag() {
  dragDepth = 0;
  $("drop").hidden = true;
}
addEventListener("dragleave", () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) endDrag();
});
// Only links and files drop here; a host being reordered drops on its list.
addEventListener("dragover", (e) => {
  if (carriesData(e)) e.preventDefault();
});
// Dropped links and files fill the Add dialog, like pasted ones, unless the
// settings say to add them at once.
addEventListener("drop", (e) => {
  endDrag();
  if (!carriesData(e)) return;
  const data = e.dataTransfer;
  const now = Boolean(state?.skip_add_dialog);
  if (data.files.length) {
    e.preventDefault();
    (now ? importFiles : readFiles)(data.files);
  } else if (!e.target.closest?.("textarea, input")) {
    e.preventDefault();
    const text = data.getData("text/uri-list") || data.getData("text/plain");
    now ? addText(text) : openAdd(text);
  }
});

// ---- live state

function connect() {
  const events = new EventSource("/api/events");
  let timer = null;
  events.addEventListener("state", (e) => {
    clearTimeout(timer);
    timer = null;
    offline = false;
    const s = JSON.parse(e.data);
    // Another version means Haul was updated and restarted under this page,
    // whose code the new one may no longer understand.
    pageVersion ??= s.version;
    updated = s.version !== pageVersion;
    render(s);
  });
  events.addEventListener("error", () => {
    timer ??= setTimeout(() => {
      offline = true;
      if (state) render(state);
    }, 1500);
    // The browser reconnects by itself, except after an answer that is no
    // event stream (an error page while Haul starts up, say): start over.
    if (events.readyState === EventSource.CLOSED) setTimeout(connect, 3000);
  });
}

takeAddRoute();
view = currentView();
applyTheme(theme);
connect();
