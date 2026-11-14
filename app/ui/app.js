// Interface of the app on the desktop (M3) and on Android (M4). Talks to the
// commands in src-tauri/src/commands.rs and, on Android only, to the Kotlin
// layer (`plugin:lebenszeichen|…`). Tauri provides `window.__TAURI__` because
// `withGlobalTauri` is switched on.
//
// Every text from outside (names, codes, messages) goes in through
// `textContent`, never as HTML.

"use strict";

const tauri = window.__TAURI__;

/// How often the list is redrawn, so that times like "wieder möglich ab"
/// stay right without any message arriving.
const REFRESH_MS = 30_000;

const $ = (id) => document.getElementById(id);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// ---------------------------------------------------------------- Log

/// Lines that arrive this close together belong to one message.
const GROUP_MS = 300;

let lastEntry = null;
let lastEntryAt = 0;

/// Adds lines to the log, newest message on top. A message keeps the order
/// of its lines; the background fetch sends them one by one, so lines that
/// follow each other quickly are put into the same entry.
function log(...lines) {
  const now = Date.now();
  const list = $("log");
  if (!lastEntry || now - lastEntryAt > GROUP_MS || !lastEntry.isConnected) {
    lastEntry = el("li", "", "");
    list.prepend(lastEntry);
  }
  lastEntryAt = now;
  for (const line of lines) {
    if (lastEntry.textContent) lastEntry.textContent += "\n";
    lastEntry.textContent += line;
  }
  while (list.children.length > 50) list.lastChild.remove();
}

/// Starts a new log entry, for the reply of a command.
function logReply(lines) {
  if (lines.length === 0) return;
  lastEntry = null;
  log(...lines);
  lastEntry = null;
}

// ---------------------------------------------------------------- Commands

function showError(text) {
  $("error-text").textContent = text;
  $("error").hidden = !text;
}

$("error-close").addEventListener("click", () => showError(""));

/// Runs a command, logs its text and reports failures.
async function call(command, args = {}) {
  showError("");
  try {
    const reply = await tauri.core.invoke(command, args);
    logReply(reply.lines);
    return reply.value;
  } catch (error) {
    showError(String(error));
    throw error;
  }
}

function button(text, onClick, className = "") {
  const node = el("button", className, text);
  node.type = "button";
  node.addEventListener("click", async () => {
    node.disabled = true;
    try {
      await onClick();
    } catch {
      // Already shown by `call`.
    } finally {
      node.disabled = false;
      await refresh();
    }
  });
  return node;
}

/// How long a button waits for its second click.
const ARM_MS = 5000;

/// Buttons waiting for their second click, by key, with the time of the
/// first click. Kept outside the buttons because the list is redrawn often.
const armed = new Map();

/// A button that asks once more before it acts, without a dialog.
function confirmButton(key, text, question, onClick, className = "danger") {
  const isArmed = () => Date.now() - (armed.get(key) ?? 0) < ARM_MS;
  const node = el("button", className, isArmed() ? question : text);
  node.type = "button";
  node.classList.toggle("armed", isArmed());
  node.addEventListener("click", async () => {
    if (!isArmed()) {
      armed.set(key, Date.now());
      node.textContent = question;
      node.classList.add("armed");
      setTimeout(() => {
        armed.delete(key);
        refresh();
      }, ARM_MS);
      return;
    }
    armed.delete(key);
    node.disabled = true;
    try {
      await onClick();
    } catch {
      // Already shown by `call`.
    } finally {
      await refresh();
    }
  });
  return node;
}

// ---------------------------------------------------------------- Panels

const PANELS = ["invite", "accept", "device"];

/// Opens one of the panels in the header, or closes all with `null`.
function openPanel(name) {
  for (const panel of PANELS) {
    const open = panel === name;
    $(`panel-${panel}`).hidden = !open;
    for (const toggle of document.querySelectorAll(`[data-panel="${panel}"]`)) {
      toggle.setAttribute("aria-expanded", String(open));
    }
  }
  if (name === "device") {
    showDevice();
  } else if (name) {
    $(`panel-${name}`).querySelector("input")?.focus();
  }
}

for (const toggle of document.querySelectorAll("[data-panel]")) {
  toggle.addEventListener("click", () => {
    const panel = toggle.dataset.panel;
    openPanel($(`panel-${panel}`).hidden ? panel : null);
  });
}

for (const close of document.querySelectorAll("[data-close]")) {
  close.addEventListener("click", () => openPanel(null));
}

// ---------------------------------------------------------------- Device (Android)

const PLUGIN = "plugin:lebenszeichen|";

/// Android's importance of a channel that may show banners and full screen.
const IMPORTANCE_HIGH = 4;

/// A line of the checklist: state, text, and buttons to fix it.
function check(ok, text, ...buttons) {
  const item = el("li", ok ? "done" : "missing");
  item.append(el("span", "state", ok ? "✓" : "✗"), el("span", "", text));
  if (!ok) item.append(...buttons);
  return item;
}

/// A button for a command of the Kotlin layer, which reports no lines.
function pluginButton(text, command, args = {}, className = "") {
  return button(
    text,
    async () => {
      try {
        await tauri.core.invoke(PLUGIN + command, args);
      } catch (error) {
        showError(String(error));
      }
      await showDevice();
    },
    className,
  );
}

/// Reads what Android allows and fills the checklist.
async function showDevice() {
  let status;
  try {
    status = await tauri.core.invoke(PLUGIN + "status");
  } catch (error) {
    showError(String(error));
    return;
  }
  const distributors = status.distributors ?? [];
  let push;
  if (status.distributorAcknowledged && status.endpoint) {
    push = check(true, `Wecksignal: angemeldet bei ${status.distributor}`);
  } else if (distributors.length === 0) {
    push = check(false, "Wecksignal: keine UnifiedPush-App gefunden. Installiere ntfy und öffne sie einmal.");
  } else {
    push = check(false, "Wecksignal: nicht angemeldet", pluginButton("Anmelden", "register"));
  }
  const channelOk = status.channelImportance < 0 || status.channelImportance >= IMPORTANCE_HIGH;
  $("device-checks").replaceChildren(
    push,
    check(
      status.notificationsEnabled,
      "Benachrichtigungen erlaubt",
      pluginButton("Erlauben", "request_notification_permission"),
      pluginButton("Einstellungen", "open_settings", { target: "notifications" }, "quiet"),
    ),
    check(
      channelOk,
      "Kanal „Anfragen“ mit hoher Wichtigkeit",
      pluginButton("Einstellungen", "open_settings", { target: "notifications" }),
    ),
    check(
      status.fullScreenIntentAllowed,
      "Vollbild-Anzeige erlaubt",
      pluginButton("Einstellungen", "open_settings", { target: "fullscreen" }),
    ),
    check(
      status.batteryUnrestricted,
      "Von der Akku-Optimierung ausgenommen",
      pluginButton("Ausnehmen", "open_settings", { target: "battery" }),
    ),
  );
}

// Back from the system settings: show what changed.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && !$("panel-device").hidden) showDevice();
});

tauri.core.invoke("platform").then((platform) => {
  document.documentElement.dataset.platform = platform;
  $("device-toggle").hidden = platform !== "android";
});

// ---------------------------------------------------------------- Settings

/// Names of contacts whose settings are unfolded, and values typed there but
/// not saved yet. Kept outside the cards because the list is redrawn often.
const openSettings = new Set();
const drafts = new Map();

/// The fields of the settings: key in `drafts`, label, argument of
/// `set_rules`, key in `contact.rules`, and the limits of the input.
const RULE_FIELDS = [
  ["interval", "Mindestabstand nach einer Antwort (Stunden)", "intervalHours", "interval_hours", 0, 168, 0.5],
  ["expiry", "Unbeantwortet abgelaufen nach (Stunden)", "expiryHours", "expiry_hours", 0.5, 168, 0.5],
  ["weekly", "Höchstens pro Woche (0: gar nicht)", "weeklyLimit", "weekly_limit", 0, 999, 1],
];

/// Settings of a paired contact: how often the contact may ask, how often
/// the own side may ask the contact, and ending the connection.
function settings(contact) {
  const name = contact.name;
  const rules = contact.rules;
  const draft = drafts.get(name) ?? {};
  const box = el("div", "settings");
  box.id = `settings-${name}`;
  box.hidden = !openSettings.has(name);

  const form = el("form", "rules");
  form.append(el("h3", "wide-field", `${name} darf dich fragen`));
  for (const [key, text, , rule, min, max, step] of RULE_FIELDS) {
    const input = el("input");
    input.type = "number";
    input.name = key;
    input.min = min;
    input.max = max;
    input.step = step;
    input.required = true;
    input.value = draft[key] ?? rules[rule];
    input.addEventListener("input", () => {
      drafts.set(name, { ...(drafts.get(name) ?? {}), [key]: input.value });
    });
    const label = el("label", "", text);
    label.append(input);
    form.append(label);
  }
  const save = el("button", "wide-field", "Speichern");
  save.type = "submit";
  form.append(save);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    // Only what changed goes out; the other values stay as they are.
    const args = { name };
    for (const [key, , arg, rule] of RULE_FIELDS) {
      const value = Number(form.elements[key].value);
      args[arg] = value === rules[rule] ? null : value;
    }
    save.disabled = true;
    try {
      if (RULE_FIELDS.some(([, , arg]) => args[arg] !== null)) {
        await call("set_rules", args);
      }
      drafts.delete(name);
    } catch {
      // Already shown by `call`.
    } finally {
      save.disabled = false;
      await refresh();
    }
  });

  const danger = el("div", "danger-zone");
  danger.append(
    confirmButton(`remove:${name}`, "Verbindung beenden", "Wirklich? Nochmal klicken", () =>
      call("remove", { name }),
    ),
  );

  box.append(form, el("p", "note", `Du darfst ${name} fragen: ${rules.peer}`), danger);
  return box;
}

/// The ⚙ button that folds the settings of a contact in and out.
function settingsToggle(name) {
  const toggle = el("button", "icon", "⚙");
  toggle.type = "button";
  toggle.setAttribute("aria-label", `Einstellungen für ${name}`);
  toggle.setAttribute("aria-controls", `settings-${name}`);
  toggle.setAttribute("aria-expanded", String(openSettings.has(name)));
  toggle.addEventListener("click", () => {
    const box = document.getElementById(`settings-${name}`);
    const open = box.hidden;
    box.hidden = !open;
    toggle.setAttribute("aria-expanded", String(open));
    if (open) openSettings.add(name);
    else openSettings.delete(name);
  });
  return toggle;
}

// ---------------------------------------------------------------- Cards

/// "123456" as "123 456", easier to compare.
function grouped(code) {
  return code.replace(/^(\d{3})(\d{3})$/, "$1 $2");
}

function row(...children) {
  const node = el("div", "row");
  node.append(...children);
  return node;
}

function spacer() {
  return el("span", "spacer");
}

function contactCard(contact) {
  const name = contact.name;
  const item = el("li", "card");

  switch (contact.status) {
    case "awaiting_confirmation":
      item.classList.add("pairing");
      item.append(
        el("h3", "", name),
        el("p", "", `${name} hat deine Einladung angenommen. Vergleicht den Prüfcode:`),
        el("p", "verification", grouped(contact.code)),
        row(
          button("Prüfcode stimmt: bestätigen", () => call("confirm", { name }), "primary"),
          button("Stimmt nicht: ablehnen", () => call("reject", { name }), "quiet"),
        ),
      );
      return item;

    case "awaiting_hello_ok":
      item.classList.add("pairing");
      item.append(
        el("h3", "", name),
        el("p", "", `Wartet auf die Bestätigung durch ${name}. Vergleicht den Prüfcode:`),
        el("p", "verification", grouped(contact.code)),
        row(button("Stimmt nicht: ablehnen", () => call("reject", { name }), "quiet")),
      );
      return item;

    case "ended":
      item.classList.add("ended");
      item.append(
        el("h3", "", name),
        el("p", "status", "Die Verbindung ist beendet."),
        row(
          confirmButton(`remove:${name}`, "Entfernen", "Wirklich? Nochmal klicken", () =>
            call("remove", { name }),
          ),
        ),
      );
      return item;
  }

  if (contact.incoming) {
    item.classList.add("request");
    item.append(
      el("h3", "", name),
      el("p", "", `${name} ${contact.incoming}`),
      button("Lebenszeichen senden ✓", () => call("respond", { name }), "primary wide"),
    );
  } else {
    item.append(el("h3", "", name));
  }

  if (contact.last_response) {
    const last = el("p", "status", "letzte Antwort: ");
    last.append(el("span", contact.last_response.startsWith("✓") ? "ok" : "", contact.last_response));
    item.append(last);
  }
  if (contact.outgoing) item.append(el("p", "status", contact.outgoing));

  const ask = button("Lebenszeichen?", () => call("request", { name }));
  ask.disabled = !contact.can_request;
  const parts = [ask];
  if (contact.next_request) parts.push(el("span", "note", contact.next_request));
  parts.push(spacer(), settingsToggle(name));
  item.append(row(...parts), settings(contact));
  return item;
}

/// Open requests first, then pairings, then the others; ended ones last.
function rank(contact) {
  if (contact.status === "paired" && contact.incoming) return 0;
  if (contact.status.startsWith("awaiting")) return 1;
  if (contact.status === "ended") return 3;
  return 2;
}

function invitationCard(invitation) {
  const name = invitation.name;
  const item = el("li", "card invitation");
  item.append(
    row(
      el("h3", "", name),
      spacer(),
      confirmButton(
        `withdraw:${name}`,
        "Zurückziehen",
        "Wirklich? Nochmal klicken",
        () => call("remove", { name }),
        "quiet",
      ),
    ),
    el("p", "note", invitation.text),
  );
  return item;
}

// ---------------------------------------------------------------- Refresh

/// Counts calls of `refresh`, so that a slow, older one cannot overwrite the
/// result of a newer one.
let refreshes = 0;

async function refresh() {
  const ticket = ++refreshes;
  const setup = await tauri.core.invoke("setup_state");
  if (ticket !== refreshes) return;
  $("setup").hidden = setup.ready;
  $("app").hidden = !setup.ready;
  $("actions").hidden = !setup.ready;
  $("state-path").textContent = `Zustand: ${setup.path}`;
  if (!setup.ready) return;
  let cards;
  try {
    cards = await tauri.core.invoke("overview").then((reply) => reply.value);
  } catch (error) {
    showError(String(error));
    return;
  }
  if (ticket !== refreshes) return;
  // Redrawing would take the cursor out of a field being typed in; wait
  // until it is left.
  if (editing()) {
    redrawLater = true;
    return;
  }
  const contacts = [...cards.contacts].sort((a, b) => rank(a) - rank(b));
  $("contacts").replaceChildren(...contacts.map(contactCard));
  $("invitations").replaceChildren(...cards.invitations.map(invitationCard));
  $("invitations-section").hidden = cards.invitations.length === 0;
  $("no-contacts").hidden = cards.contacts.length + cards.invitations.length > 0;
}

let redrawLater = false;

function editing() {
  const active = document.activeElement;
  return active?.tagName === "INPUT" && $("contacts").contains(active);
}

$("contacts").addEventListener("focusout", () => {
  // `activeElement` changes only after this event.
  setTimeout(() => {
    if (redrawLater && !editing()) {
      redrawLater = false;
      refresh();
    }
  });
});

// ---------------------------------------------------------------- Forms

function onSubmit(id, handler) {
  const form = $(id);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const submit = form.querySelector("button[type=submit]");
    submit.disabled = true;
    try {
      await handler(new FormData(form), form);
    } catch {
      // Already shown by `call`.
    } finally {
      submit.disabled = false;
      await refresh();
    }
  });
}

onSubmit("setup-form", async (data) => {
  await call("init_device", {
    relay: data.get("relay"),
    accessToken: data.get("access_token"),
  });
});

/// Modules of light margin around a QR code, as in `lz_client::qr`.
const QR_QUIET_ZONE = 4;

/// Draws a QR code as one SVG path, one unit per module. Built through the
/// DOM, so the Content Security Policy needs no exception for images.
function qrSvg(qr) {
  const ns = "http://www.w3.org/2000/svg";
  const side = qr.width + 2 * QR_QUIET_ZONE;
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${side} ${side}`);
  svg.setAttribute("shape-rendering", "crispEdges");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "QR-Code der Einladung");
  const background = document.createElementNS(ns, "rect");
  background.setAttribute("width", side);
  background.setAttribute("height", side);
  background.setAttribute("fill", "#fff");
  let d = "";
  qr.dark.forEach((dark, i) => {
    if (!dark) return;
    const x = (i % qr.width) + QR_QUIET_ZONE;
    const y = Math.floor(i / qr.width) + QR_QUIET_ZONE;
    d += `M${x} ${y}h1v1h-1z`;
  });
  const modules = document.createElementNS(ns, "path");
  modules.setAttribute("d", d);
  modules.setAttribute("fill", "#000");
  svg.append(background, modules);
  return svg;
}

onSubmit("invite-form", async (data, form) => {
  const invitation = await call("invite", {
    name: data.get("name"),
    voucher: data.get("voucher") === "on",
  });
  $("invite-qr").replaceChildren(qrSvg(invitation.qr));
  $("invite-code").value = invitation.code;
  $("invite-result").hidden = false;
  form.reset();
});

// A click selects the whole code, ready to copy.
$("invite-code").addEventListener("focus", (event) => event.target.select());

onSubmit("accept-form", async (data, form) => {
  const name = data.get("name").trim();
  const result = $("accept-result");
  result.hidden = true;
  const code = await call("accept", { name, code: data.get("code") });
  result.textContent =
    `Angenommen. Prüfcode: ${grouped(code)}. Vergleiche ihn mit dem Prüfcode bei ` +
    `${name}; gekoppelt seid ihr, sobald ${name} bestätigt hat.`;
  result.hidden = false;
  form.reset();
});

$("sync").addEventListener("click", async () => {
  try {
    await call("sync");
  } catch {
    // Already shown by `call`.
  }
  await refresh();
});

tauri.event.listen("lz-line", (event) => {
  log(event.payload);
  refresh();
});

refresh();
setInterval(refresh, REFRESH_MS);
