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
const shownHost = (it) => it.host || it.mirrors[0] || "";

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

function toast(message, kind = "info") {
  const names = { info: "info", success: "checkmark-circle", error: "error-circle" };
  const node = el("div", { class: `toast ${kind}` }, icon(names[kind] ?? "info"), el("div", {}, message));
  const box = $("toasts");
  box.append(node);
  while (box.childElementCount > 4) box.firstElementChild.remove();
  setTimeout(() => node.remove(), kind === "error" ? 7000 : 4000);
}

const report = (error) => toast(error.message, "error");
const act = (id, action) => api(`/api/item/${encodeURIComponent(id)}/${action}`).catch(report);

function addedMessage(r) {
  const parts = [`${r.added} added`];
  if (r.merged) parts.push(`${r.merged} merged as mirrors`);
  if (r.duplicates) parts.push(`${r.duplicates} already in the list`);
  if (r.invalid) parts.push(`${r.invalid} not http(s)`);
  return parts.join(" · ");
}

// ---- views

const ACTIVE = ["downloading", "resolving", "waiting", "queued", "checking"];

const LISTS = {
  all: { label: "All downloads", icon: "apps-list", test: () => true },
  active: { label: "Active", icon: "arrow-circle-down", test: (i) => ACTIVE.includes(i.status) },
  attention: { label: "Needs you", icon: "warning", test: (i) => i.status === "captcha", badge: "attention" },
  failed: { label: "Failed", icon: "error-circle", test: (i) => i.status === "error", badge: "critical" },
  paused: { label: "Paused", icon: "pause-circle", test: (i) => i.status === "paused" },
  done: { label: "Completed", icon: "checkmark-circle", test: (i) => i.status === "done" },
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

function currentView() {
  const name = location.hash.replace(/^#\/?/, "");
  return name in LISTS || name in PAGES ? name : "all";
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
  view = currentView();
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

// ---- render

function render(s) {
  state = s;
  if (s.theme !== theme) applyTheme(s.theme);
  renderNav(s);
  renderHead(s);
  const isList = view in LISTS;
  $("view-downloads").hidden = !isList;
  $("view-hosts").hidden = view !== "hosts";
  $("view-settings").hidden = view !== "settings";
  if (isList) {
    renderBanner(s);
    renderList(s);
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
    for (const [key, spec] of Object.entries(LISTS)) {
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
  for (const [key, item] of navItems) {
    const selected = key === view;
    if (selected) item.node.setAttribute("aria-current", "page");
    else item.node.removeAttribute("aria-current");
    if (item.filled !== selected) {
      item.filled = selected;
      item.node.firstChild.replaceWith(icon(selected ? `${item.spec.icon}-filled` : item.spec.icon));
    }
    if (!item.spec.test) continue;
    const count = s.items.filter(item.spec.test).length;
    setText(item.badge, count ? String(count) : "");
    item.badge.className = `badge${count && item.spec.badge ? ` ${item.spec.badge}` : ""}`;
    item.node.classList.toggle("attention", count > 0 && Boolean(item.spec.badge));
    item.node.classList.toggle("critical", count > 0 && item.spec.badge === "critical");
  }
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
  const spec = LISTS[view] ?? PAGES[view];
  setText($("title"), view === "all" ? "Downloads" : spec.label);
  $("commands").hidden = !(view in LISTS);
  const failed = s.items.some((i) => i.status === "error");
  const done = s.items.some((i) => i.status === "done");
  const key = `${s.paused}|${failed}|${done}`;
  if (key === commandsKey) return;
  commandsKey = key;
  if (!commandButtons) makeCommands();
  // Only the buttons are rebuilt, so typing in the search box keeps focus.
  commandButtons.replaceChildren(
    button("Add links", { icon: "add", kind: "accent", onclick: () => openAdd() }),
    button(s.paused ? "Resume all" : "Pause all", {
      icon: s.paused ? "play" : "pause",
      kind: "subtle",
      onclick: () => api(`/api/global/${s.paused ? "resume" : "pause"}`).catch(report),
    }),
    button("Retry failed", {
      icon: "arrow-clockwise",
      kind: "subtle",
      disabled: !failed,
      onclick: () => api("/api/global/retry_failed").catch(report),
    }),
    button("Clear completed", {
      icon: "broom",
      kind: "subtle",
      disabled: !done,
      onclick: () => api("/api/global/clear_done").catch(report),
    }),
  );
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
  const key = `${offline}|${s.paused}|${s.config_error}`;
  if (key === bannerKey) return;
  bannerKey = key;
  const bars = [];
  if (offline) {
    bars.push(infobar("critical", "error-circle", "Lost the connection to Haul.", "Reconnecting…"));
  }
  if (s.config_error) {
    bars.push(
      infobar("critical", "error-circle", "The settings file has an error.", "The previous settings stay in effect until it is fixed.", [
        button("Show", { onclick: () => go("settings") }),
      ]),
    );
  }
  if (s.paused) {
    bars.push(
      infobar("info", "pause-circle", "Downloads are paused.", "Nothing new starts until you resume.", [
        button("Resume all", { icon: "play", onclick: () => api("/api/global/resume").catch(report) }),
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

function caption(it) {
  const of = it.size ? `${bytes(it.done)} of ${bytes(it.size)}` : it.done ? bytes(it.done) : "";
  const pct = it.size ? ` · ${Math.floor((it.done / it.size) * 100)}%` : "";
  switch (it.status) {
    case "downloading":
      return of ? `${of}${pct}` : "Downloading";
    case "queued":
      return it.size ? `Queued · ${bytes(it.size)}` : "Queued";
    case "checking":
      return "Checking the link…";
    case "resolving":
      return "Starting…";
    case "waiting":
      return it.wait ? `Retrying in ${duration(it.wait)}` : "Waiting";
    case "paused":
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
  else if (it.status === "paused") out.push(subtle("Resume", "play", () => act(it.id, "resume")));
  else if (it.status === "error") out.push(subtle("Retry", "arrow-clockwise", () => act(it.id, "retry")));
  if (it.status === "done" && it.path) {
    out.push(subtle("Show in folder", "folder-open", () => act(it.id, "reveal")));
  }
  if (it.status === "done" && it.path && s.recycle_on_remove) {
    const recycle = () =>
      api(`/api/item/${encodeURIComponent(it.id)}/remove`).then(() => toast(`Moved ${it.name} to the ${s.bin}`, "success"), report);
    out.push(subtle(`Move to the ${s.bin}`, "delete", recycle));
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
        onclick: () =>
          api(`/api/item/${encodeURIComponent(it.id)}/browse`, { user_agent: navigator.userAgent }).catch(report),
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
      r.glyph,
      el("div", { class: "title" }, r.name, r.sub),
      el("div", { class: "progress" }, el("div", { class: "bar" }, r.fill), r.caption),
      r.rate,
      r.actions,
    ),
  );
  return r;
}

function updateRow(r, it, s) {
  const rowClass = `row s-${it.status}`;
  if (r.node.className !== rowClass) r.node.className = rowClass;
  if (r.glyphKey !== it.status) {
    r.glyphKey = it.status;
    r.glyph.replaceChildren(icon(GLYPHS[it.status] ?? "clock"));
    r.glyph.title = caption(it);
  }
  setText(r.name, it.name);
  r.name.title = it.name;

  const sub = [shownHost(it)];
  if (it.mirrors.length > 1) sub.push(`${it.mirrors.length} mirrors`);
  if (it.package) sub.push(it.package);
  const error = ["error", "waiting"].includes(it.status) ? withoutHost(it.error, sub[0]) : "";
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

  const actionsKey = `${it.status}|${Boolean(it.path)}|${s.recycle_on_remove}`;
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
  return [it.name, it.package, it.host, ...it.mirrors].some((v) => v?.toLowerCase().includes(query));
}

function renderList(s) {
  const list = $("list");
  const spec = LISTS[view];
  const visible = s.items.filter((i) => spec.test(i) && matchesQuery(i));
  const seen = new Set();
  let previous = null;
  for (const it of visible) {
    seen.add(it.id);
    let r = rows.get(it.id);
    if (!r) {
      r = makeRow();
      rows.set(it.id, r);
    }
    updateRow(r, it, s);
    const want = previous ? previous.node.nextSibling : list.firstChild;
    if (want !== r.node) list.insertBefore(r.node, want);
    previous = r;
  }
  for (const [id, r] of rows) {
    if (!seen.has(id)) {
      r.node.remove();
      rows.delete(id);
    }
  }
  renderEmpty(s, visible.length);
}

let emptyKey = null;

function renderEmpty(s, shown) {
  const box = $("empty");
  box.hidden = shown > 0;
  const key = shown ? "" : `${view}|${s.items.length > 0}|${query}`;
  if (shown || key === emptyKey) return;
  emptyKey = key;
  // With no downloads at all, the empty list is the drop target, and looks it.
  const dropzone = !query && (!s.items.length || view === "all");
  box.classList.toggle("dropzone", dropzone);
  if (query) {
    box.replaceChildren(icon("search"), el("strong", {}, "No matches"), `Nothing here matches “${query}”.`);
  } else if (dropzone) {
    box.replaceChildren(
      icon("arrow-download"),
      el("strong", {}, "Drop links or files here"),
      "Or paste links anywhere with Ctrl+V. Files can be .dlc containers or text files with links in them.",
    );
  } else {
    const [title, text] = EMPTY[view];
    box.replaceChildren(icon(LISTS[view].icon), el("strong", {}, title), text);
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

function setLimit(name, delta) {
  const host = state?.hosts.find((h) => h.name === name);
  if (host) api(`/api/host/${encodeURIComponent(name)}/limit/${Math.max(1, host.limit + delta)}`).catch(report);
}

// A button that wants a second click within a few seconds.
function confirmButton(label, confirmLabel, action) {
  const node = button(label, {});
  let timer = null;
  const reset = () => {
    timer = null;
    node.className = "button";
    node.lastChild.textContent = label;
  };
  node.addEventListener("click", () => {
    if (!timer) {
      node.className = "button danger";
      node.lastChild.textContent = confirmLabel;
      timer = setTimeout(reset, 4000);
      return;
    }
    clearTimeout(timer);
    reset();
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

function makeHostCard(name) {
  const c = { key: null, preferred: false };
  c.star = button("Prefer this host", { icon: "star", kind: "subtle small", iconOnly: true, onclick: () => setPreferred(name, !c.preferred) });
  c.stats = el("div", { class: "sub" });
  c.pills = el("span", { class: "controls" });
  c.slots = el("output");
  c.node = el(
    "div",
    { class: "card with-bar" },
    icon("server"),
    el("div", { class: "text" }, el("div", {}, name), c.stats),
    c.pills,
    el(
      "span",
      { class: "stepper", title: "Files downloaded from this host at the same time" },
      button("Fewer slots", { icon: "subtract", kind: "subtle small", iconOnly: true, onclick: () => setLimit(name, -1) }),
      c.slots,
      button("More slots", { icon: "add", kind: "subtle small", iconOnly: true, onclick: () => setLimit(name, 1) }),
    ),
    c.star,
    button("Folder for this host's files", { icon: "folder", kind: "subtle small", iconOnly: true, onclick: () => editFolder(c, name) }),
  );
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

function renderHosts(s) {
  const box = $("hosts");
  let previous = null;
  const seen = new Set();
  const fastest = s.hosts.reduce((a, h) => (h.speed > (a?.speed ?? 0) ? h : a), null)?.name;
  const preferredCount = s.hosts.filter((h) => h.preferred !== null).length;
  s.hosts.forEach((h, index) => {
    seen.add(h.name);
    let c = hostCards.get(h.name);
    if (!c) {
      c = makeHostCard(h.name);
      hostCards.set(h.name, c);
    }
    const stats = [h.speed ? `${bytes(h.speed)}/s per file` : "Speed not measured yet", `${h.ok} done`, `${h.fails} failed`, `${bytes(h.bytes)} total`];
    if (h.cooldown) stats.push(`cooling down ${duration(h.cooldown)}`);
    setText(c.stats, stats.join(" · "));
    setText(c.slots, `${h.active} / ${h.limit}`);
    const best = h.name === fastest;
    const key = `${h.blocked}|${h.route}|${s.proxy}|${best}|${h.folder}|${h.preferred}|${preferredCount}`;
    if (c.key !== key) {
      c.key = key;
      c.node.classList.toggle("best", best);
      const pills = [];
      c.preferred = h.preferred !== null;
      const starTitle = c.preferred ? "Preferred: click to stop preferring it" : "Prefer this host";
      c.star.replaceChildren(icon(c.preferred ? "star-filled" : "star"));
      c.star.title = starTitle;
      c.star.setAttribute("aria-label", starTitle);
      c.star.setAttribute("aria-pressed", String(c.preferred));
      if (c.preferred) pills.push(el("span", { class: "pill preferred" }, icon("star-filled"), preferredCount > 1 ? `Preferred · ${h.preferred + 1}` : "Preferred"));
      if (best) pills.push(el("span", { class: "pill success" }, "Fastest"));
      if (h.folder) pills.push(el("span", { class: "pill", title: "Its files go to this subfolder" }, icon("folder"), h.folder));
      if (s.proxy) pills.push(el("span", { class: `pill${h.route === "direct" ? " caution" : ""}` }, icon("shield"), `via ${h.route}`));
      c.pills.replaceChildren(...pills);
      for (const bar of c.node.querySelectorAll(":scope > .infobar")) bar.remove();
      for (const bar of routeBars(h, s)) c.node.append(bar);
    }
    const want = previous ? previous.node.nextSibling : box.firstChild;
    if (want !== c.node) box.insertBefore(c.node, want);
    previous = c;
  });
  for (const [name, c] of hostCards) {
    if (!seen.has(name)) {
      c.node.remove();
      hostCards.delete(name);
    }
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
  if (!secret) return input;
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

// The last two parts of a folder's path, which are enough to recognise it.
function shortPath(path) {
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts.length > 2 ? parts.slice(-2).join(sep) : path;
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
    ),
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
  );
}

// ---- status bar

let statusKey = null;

function renderStatus(s) {
  const downloading = s.items.filter((i) => i.status === "downloading").length;
  const queued = s.items.filter((i) => ["queued", "waiting", "checking", "resolving"].includes(i.status)).length;
  const clipboard = s.desktop && s.watch_clipboard;
  const key = JSON.stringify([bytes(s.speed), downloading, queued, s.rate, s.proxy, theme, s.paused, clipboard, s.download_dir]);
  if (key === statusKey) return;
  statusKey = key;
  const themeSpec = THEMES.find(([value]) => value === theme) ?? THEMES[0];
  const next = THEMES[(THEMES.indexOf(themeSpec) + 1) % THEMES.length][0];
  fill(
    $("status"),
    // Where the files go, on every page; one click opens it.
    el(
      "button",
      {
        type: "button",
        class: "status-item",
        title: `Open the download folder: ${s.download_dir}`,
        "aria-label": `Open the download folder, ${s.download_dir}`,
        onclick: () => openPlace("downloads"),
      },
      icon("folder-open"),
      el("span", { class: "status-label" }, shortPath(s.download_dir)),
    ),
    el("span", { class: `status-item${s.speed ? " on" : ""}` }, icon("arrow-download"), `${bytes(s.speed)}/s`),
    el("span", { class: "status-item" }, s.paused ? "Paused" : `${downloading} downloading · ${queued} waiting`),
    el("span", { class: "grow" }),
    clipboard
      ? el(
          "button",
          { type: "button", class: "status-item on", title: "Links you copy are added (Settings)", onclick: () => go("settings") },
          icon("clipboard-link"),
          "Watching the clipboard",
        )
      : null,
    el(
      "button",
      { type: "button", class: "status-item", title: "Speed limit (Settings)", onclick: () => go("settings") },
      icon("top-speed"),
      s.rate ? `${(s.rate / 1e6).toFixed(1)} MB/s cap` : "No speed limit",
    ),
    el(
      "button",
      { type: "button", class: `status-item${s.proxy ? " on" : ""}`, title: "Proxy (Settings)", onclick: () => go("settings") },
      icon("shield"),
      s.proxy ? "Proxy on" : "Proxy off",
    ),
    el(
      "button",
      { type: "button", class: "status-item", title: `Theme: ${themeSpec[1]} (click for ${next})`, onclick: () => setTheme(next) },
      icon(themeSpec[2]),
      themeSpec[1],
    ),
  );
}

// ---- adding links

const dialog = $("add-dialog");

function openAdd(text = "") {
  if (text) {
    const current = $("add-text").value.trim();
    $("add-text").value = current ? `${current}\n${text}` : text;
  }
  if (!dialog.open) dialog.showModal();
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
    if (r.package && !$("add-package").value.trim()) $("add-package").value = r.package;
    openAdd(r.links.join("\n"));
  } catch (error) {
    toast(`Import failed: ${error.message}`, "error");
  }
}

$("add-import").prepend(icon("arrow-upload"), "Import file…");
$("add-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (await addText($("add-text").value, $("add-package").value.trim())) {
    $("add-text").value = "";
    dialog.close();
  }
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

// Paste links anywhere outside a text field.
document.addEventListener("paste", (e) => {
  if (e.target.closest?.("input, textarea") || dialog.open) return;
  const text = e.clipboardData?.getData("text") ?? "";
  if (/https?:\/\//i.test(text)) {
    e.preventDefault();
    openAdd(text);
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "/" && !e.target.closest?.("input, textarea") && !dialog.open && view in LISTS) {
    e.preventDefault();
    $("search")?.focus();
  }
});

// Drop links or container files anywhere.
let dragDepth = 0;
const carriesData = (e) => [...(e.dataTransfer?.types ?? [])].some((t) => t === "Files" || t.startsWith("text/"));
$("drop").firstElementChild.append(
  icon("arrow-download"),
  el("strong", {}, "Drop to add to Haul"),
  "Links, .dlc containers and text files with links in them.",
);
// The empty list's drop zone lights up while it is on screen; otherwise the
// whole window turns into one, the same way.
addEventListener("dragenter", (e) => {
  if (!carriesData(e) || dialog.open) return;
  dragDepth += 1;
  const zone = $("empty");
  if (zone.classList.contains("dropzone") && zone.offsetParent) zone.classList.add("over");
  else $("drop").hidden = false;
});
function endDrag() {
  dragDepth = 0;
  $("drop").hidden = true;
  $("empty").classList.remove("over");
}
addEventListener("dragleave", () => {
  dragDepth = Math.max(0, dragDepth - 1);
  if (!dragDepth) endDrag();
});
addEventListener("dragover", (e) => e.preventDefault());
// Dropped links and files fill the Add dialog, like pasted ones, unless the
// settings say to add them at once.
addEventListener("drop", (e) => {
  endDrag();
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
    render(JSON.parse(e.data));
  });
  events.addEventListener("error", () => {
    timer ??= setTimeout(() => {
      offline = true;
      if (state) render(state);
    }, 1500);
  });
}

takeAddRoute();
view = currentView();
applyTheme(theme);
connect();
