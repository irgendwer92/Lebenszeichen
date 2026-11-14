// Desktop interface (M3). Talks to the commands in src-tauri/src/desktop.rs.
// Tauri provides `window.__TAURI__` because `withGlobalTauri` is switched on.
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

function showError(text) {
  const box = $("error");
  box.textContent = text;
  box.hidden = !text;
}

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
function confirmButton(key, text, question, onClick) {
  const isArmed = () => Date.now() - (armed.get(key) ?? 0) < ARM_MS;
  const node = el("button", "quiet", isArmed() ? question : text);
  node.type = "button";
  node.addEventListener("click", async () => {
    if (!isArmed()) {
      armed.set(key, Date.now());
      node.textContent = question;
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

/// "Einstellungen" of a paired contact: how often the contact may ask, and
/// how often the own side may ask the contact.
function settings(contact) {
  const name = contact.name;
  const rules = contact.rules;
  const draft = drafts.get(name) ?? {};
  const details = el("details", "settings");
  details.open = openSettings.has(name);
  details.addEventListener("toggle", () => {
    if (details.open) openSettings.add(name);
    else openSettings.delete(name);
  });
  details.append(el("summary", "", "Einstellungen"));

  const form = el("form", "rules");
  form.append(el("p", "", `${name} darf dich fragen:`));
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
  const save = el("button", "", "Speichern");
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

  details.append(form, el("p", "note", `Du darfst ${name} fragen: ${rules.peer}`));
  return details;
}

function contactCard(contact) {
  const item = el("li", `card ${contact.status}`);
  item.append(el("h3", "", contact.name));
  const name = contact.name;

  switch (contact.status) {
    case "awaiting_confirmation":
      item.append(
        el("p", "", `hat die Einladung angenommen. Prüfcode: ${contact.code}`),
        el("p", "note", "Vergleiche den Prüfcode mit dem bei " + name + "."),
        button("Prüfcode stimmt: bestätigen", () => call("confirm", { name })),
        button("Stimmt nicht: ablehnen", () => call("reject", { name }), "quiet"),
      );
      break;
    case "awaiting_hello_ok":
      item.append(
        el("p", "", `wartet auf die Bestätigung durch ${name}. Prüfcode: ${contact.code}`),
        button("Stimmt nicht: ablehnen", () => call("reject", { name }), "quiet"),
      );
      break;
    case "ended":
      item.append(el("p", "", "Die Verbindung ist beendet."));
      break;
    default: {
      if (contact.incoming) {
        item.append(
          el("p", "incoming", `${name} ${contact.incoming}`),
          button("Lebenszeichen senden ✓", () => call("respond", { name }), "primary"),
        );
      }
      const ask = button("Lebenszeichen?", () => call("request", { name }));
      ask.disabled = !contact.can_request;
      item.append(ask);
      if (contact.next_request) item.append(el("p", "note", contact.next_request));
      if (contact.outgoing) item.append(el("p", "note", contact.outgoing));
      if (contact.last_response) {
        item.append(el("p", "", `letzte Antwort: ${contact.last_response}`));
      }
      if (contact.rules) item.append(settings(contact));
    }
  }
  const removeText = contact.status === "ended" ? "Entfernen" : "Verbindung beenden";
  item.append(
    confirmButton(`remove:${name}`, removeText, "Wirklich? Nochmal klicken", () =>
      call("remove", { name }),
    ),
  );
  return item;
}

function invitationCard(invitation) {
  const item = el("li", "card invitation");
  const name = invitation.name;
  item.append(
    el("h3", "", name),
    el("p", "note", invitation.text),
    confirmButton(`withdraw:${name}`, "Zurückziehen", "Wirklich? Nochmal klicken", () =>
      call("remove", { name }),
    ),
  );
  return item;
}

/// Counts calls of `refresh`, so that a slow, older one cannot overwrite the
/// result of a newer one.
let refreshes = 0;

async function refresh() {
  const ticket = ++refreshes;
  const setup = await tauri.core.invoke("setup_state");
  if (ticket !== refreshes) return;
  $("setup").hidden = setup.ready;
  $("app").hidden = !setup.ready;
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
  $("contacts").replaceChildren(...cards.contacts.map(contactCard));
  $("invitations").replaceChildren(...cards.invitations.map(invitationCard));
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

onSubmit("accept-form", async (data, form) => {
  const name = data.get("name").trim();
  const result = $("accept-result");
  result.hidden = true;
  const code = await call("accept", { name, code: data.get("code") });
  result.textContent =
    `Angenommen. Prüfcode: ${code}. Vergleiche ihn mit dem Prüfcode bei ${name}; ` +
    `gekoppelt seid ihr, sobald ${name} bestätigt hat.`;
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
