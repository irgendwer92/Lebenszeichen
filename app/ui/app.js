// Interface of the app on the desktop (M3) and on Android (M4). Talks to the
// commands in src-tauri/src/commands.rs and, on Android only, to the Kotlin
// layer (`plugin:lebenszeichen|…`). Tauri provides `window.__TAURI__` because
// `withGlobalTauri` is switched on.
//
// Every text from outside (names, codes, messages) goes in through
// `textContent`, never as HTML.
//
// The texts of the interface come from the Fluent files
// (locales/*/lebenszeichen.ftl, ids `ui-…`) through the command `texts`, in
// the language of this device. Fixed texts in index.html carry `data-i18n*`
// attributes; dynamic ones go through `t("ui-…", { name: … })`. Every id is
// written out as a literal, so that a test can check that it exists.

"use strict";

const tauri = window.__TAURI__;

/// How often the list is redrawn, so that times like "possible again from"
/// stay right without any message arriving.
const REFRESH_MS = 30_000;

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- Texts

/// The texts of the language in effect, by id; empty until they have loaded.
let texts = {};

/// The text `id`, with `{ $name }` replaced by `args.name`.
function t(id, args = {}) {
  const text = texts[id] ?? id;
  return text.replace(/\{\s*\$(\w+)\s*\}/g, (whole, name) =>
    name in args ? String(args[name]) : whole,
  );
}

/// Puts the texts into the elements that carry `data-i18n*` attributes.
function applyTexts() {
  for (const node of document.querySelectorAll("[data-i18n]")) {
    node.textContent = t(node.dataset.i18n);
  }
  for (const attribute of ["aria-label", "placeholder", "title"]) {
    for (const node of document.querySelectorAll(`[data-i18n-${attribute}]`)) {
      node.setAttribute(attribute, t(node.getAttribute(`data-i18n-${attribute}`)));
    }
  }
}

/// Takes over the answer of `texts` or `set_language`.
function useTexts(answer) {
  texts = answer.texts;
  document.documentElement.lang = answer.lang;
  $("language").value = answer.language;
  applyTexts();
}

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

const PANELS = ["invite", "accept", "devices", "device"];

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
    if (isAndroid) showDevice();
  } else if (name === "devices") {
    showDevices();
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

// ---------------------------------------------------------------- This device

/// Language setting, for the desktop and Android alike. The device is the
/// owner of its language: saved in `settings.json`, never synced.
$("language").addEventListener("change", async (event) => {
  try {
    useTexts(await tauri.core.invoke("set_language", { language: event.target.value }));
  } catch (error) {
    showError(String(error));
    return;
  }
  // Everything that was drawn from texts is drawn again.
  if (isAndroid && !$("panel-device").hidden) showDevice();
  if (!$("panel-devices").hidden) showDevices();
  refresh();
});

/// "Start at login", desktop only: the row stays hidden on Android.
$("autostart").addEventListener("change", async (event) => {
  try {
    event.target.checked = await tauri.core.invoke("set_autostart", { on: event.target.checked });
  } catch (error) {
    showError(String(error));
    showAutostart();
  }
});

/// Shows whether the app starts at login.
async function showAutostart() {
  try {
    $("autostart").checked = await tauri.core.invoke("autostart");
  } catch (error) {
    showError(String(error));
  }
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
  showReportToken(status.packageName ?? "");
  showBattery();
  const distributors = status.distributors ?? [];
  let push;
  if (status.distributorAcknowledged && status.endpoint) {
    push = check(true, t("ui-check-push-on", { distributor: status.distributor }));
  } else if (distributors.length === 0) {
    push = check(false, t("ui-check-push-none"));
  } else {
    push = check(false, t("ui-check-push-off"), pluginButton(t("ui-register"), "register"));
  }
  const channelOk = status.channelImportance < 0 || status.channelImportance >= IMPORTANCE_HIGH;
  $("device-checks").replaceChildren(
    push,
    check(
      status.notificationsEnabled,
      t("ui-check-notifications"),
      pluginButton(t("ui-allow"), "request_notification_permission"),
      pluginButton(t("ui-open-settings"), "open_settings", { target: "notifications" }, "quiet"),
    ),
    check(
      channelOk,
      t("ui-check-channel"),
      pluginButton(t("ui-open-settings"), "open_settings", { target: "notifications" }),
    ),
    check(
      status.fullScreenIntentAllowed,
      t("ui-check-fullscreen"),
      pluginButton(t("ui-open-settings"), "open_settings", { target: "fullscreen" }),
    ),
    check(
      status.batteryUnrestricted,
      t("ui-check-battery"),
      pluginButton(t("ui-exempt"), "open_settings", { target: "battery" }),
    ),
  );
}

/// The switch for state reports from other apps (Android), with the token.
async function showReportToken(packageName) {
  let token;
  try {
    token = (await tauri.core.invoke("report_token")).value;
  } catch (error) {
    showError(String(error));
    return;
  }
  applyReportToken(token, packageName);
}

let reportPackage = "";

function applyReportToken(token, packageName = reportPackage) {
  reportPackage = packageName;
  $("report-token-on").checked = Boolean(token);
  $("report-token-details").hidden = !token;
  $("report-token").value = token ?? "";
  $("report-token-intent").textContent = t("ui-report-token-intent", { package: packageName });
}

async function changeReportToken(command) {
  try {
    applyReportToken(await call(command));
  } catch {
    // `call` showed the error; show what holds.
    showReportToken(reportPackage);
  }
}

$("report-token-on").addEventListener("change", (event) =>
  changeReportToken(event.target.checked ? "enable_report_token" : "disable_report_token"),
);

$("report-token-renew").addEventListener("click", () => changeReportToken("renew_report_token"));

$("report-token-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("report-token").value);
    showError("");
    $("report-token-copy").textContent = t("ui-copied");
    setTimeout(() => ($("report-token-copy").textContent = t("ui-copy")), 2000);
  } catch (error) {
    showError(String(error));
  }
});

$("report-token").addEventListener("focus", (event) => event.target.select());

/// The switch "Hand over a state when the battery is low" (Android), with
/// the reason. The core decides when the state goes out; Kotlin only reads
/// the battery level.
async function showBattery() {
  try {
    applyBattery((await tauri.core.invoke("battery_reason")).value);
  } catch (error) {
    showError(String(error));
  }
}

function applyBattery(reason) {
  const on = reason !== null && reason !== undefined;
  $("battery-on").checked = on;
  $("battery-details").hidden = !on;
  $("battery-reason").value = on ? reason : "";
}

async function changeBattery(reason) {
  try {
    applyBattery(await call("set_battery_reason", { reason }));
    // Lets the Android side start its battery check now.
    refreshNotifications();
  } catch {
    // `call` showed the error; show what holds.
    showBattery();
  }
}

$("battery-on").addEventListener("change", (event) =>
  changeBattery(event.target.checked ? t("ui-battery-default") : null),
);

$("battery-reason").addEventListener("change", (event) => {
  const reason = event.target.value.trim();
  // An empty reason would end the setting; keep the default instead.
  changeBattery(reason || t("ui-battery-default"));
});

/// Shows the event log of the Kotlin layer, newest line first: what wake
/// signals, fetches and answers did while the app was closed.
async function showDeviceLog() {
  let lines = [];
  try {
    lines = (await tauri.core.invoke(PLUGIN + "read_log")).lines ?? [];
  } catch (error) {
    showError(String(error));
  }
  $("device-log").replaceChildren(
    ...[...lines].reverse().map((line) => el("li", "", line.replaceAll("\t", "  "))),
  );
}

$("device-log").closest("details").addEventListener("toggle", (event) => {
  if (event.target.open) showDeviceLog();
});

$("device-log-clear").addEventListener("click", async () => {
  try {
    await tauri.core.invoke(PLUGIN + "clear_log");
  } catch (error) {
    showError(String(error));
  }
  await showDeviceLog();
});

/// On Android the notifications follow what the window just did.
function refreshNotifications() {
  if (document.documentElement.dataset.platform !== "android") return;
  tauri.core.invoke(PLUGIN + "refresh_requests").catch(() => {});
}

// Back from the system settings: show what changed.
document.addEventListener("visibilitychange", () => {
  if (isAndroid && !document.hidden && !$("panel-device").hidden) showDevice();
});

let isAndroid = false;

tauri.core.invoke("platform").then((platform) => {
  document.documentElement.dataset.platform = platform;
  isAndroid = platform === "android";
  // The ⚙ panel holds the language setting on both; the rest is Android's.
  $("device-android").hidden = !isAndroid;
  $("autostart-row").hidden = isAndroid;
  if (!isAndroid) showAutostart();
  $("scan").hidden = !isAndroid;
  $("setup-scan").hidden = !isAndroid;
  $("add-scan").hidden = !isAndroid;
  $("join-share").hidden = !isAndroid;
  $("join-copy").hidden = isAndroid;
});

/// What a scanned or pasted code is, in words, for "this is not what I
/// need here".
const KIND_TEXT = {
  invitation: "ui-kind-invitation",
  old_invitation: "ui-kind-old-invitation",
  device: "ui-kind-device",
  setup: "ui-kind-setup",
};

/// What the field wants, for "this is X, not Y", by the kind of code.
const WANTED_TEXT = {
  invitation: "ui-wanted-not-invitation",
  device: "ui-wanted-not-device",
  setup: "ui-wanted-not-setup",
};

/// Opens the camera. Returns the code if it is of the wanted kind; else
/// tells the person what it is and returns null.
async function scanCode(kind) {
  showError("");
  let result;
  try {
    result = await tauri.core.invoke(PLUGIN + "scan_code");
  } catch (error) {
    showError(String(error));
    return null;
  }
  if (!result?.code) return null;
  if (result.kind === "old_invitation" && kind === "invitation") {
    // Same text as `error-old-invitation` in the client.
    showError(t("ui-old-invitation"));
    return null;
  }
  if (result.kind !== kind) {
    const what = t(KIND_TEXT[result.kind] ?? "ui-kind-other");
    showError(t("ui-wrong-code", { what, wanted: t(WANTED_TEXT[kind]) }));
    return null;
  }
  return result.code;
}

/// Opens the camera; a recognized invitation lands in the code field.
$("scan").addEventListener("click", async () => {
  const code = await scanCode("invitation");
  if (!code) return;
  const form = $("accept-form");
  form.elements.code.value = code;
  const name = form.elements.name;
  (name.value.trim() ? form.querySelector("button[type=submit]") : name).focus();
});

// ---------------------------------------------------------------- Settings

/// Names of contacts whose settings are unfolded, and values typed there but
/// not saved yet. Kept outside the cards because the list is redrawn often.
const openSettings = new Set();
const drafts = new Map();

/// The fields of the settings: key in `drafts`, id of the label, argument of
/// `set_rules`, key in `contact.rules`, and the limits of the input.
const RULE_FIELDS = [
  ["interval", "ui-rule-interval", "intervalHours", "interval_hours", 0, 168, 0.5],
  ["expiry", "ui-rule-expiry", "expiryHours", "expiry_hours", 0.5, 168, 0.5],
  ["weekly", "ui-rule-weekly", "weeklyLimit", "weekly_limit", 0, 999, 1],
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
  form.append(el("h3", "wide-field", t("ui-rules-heading", { name })));
  for (const [key, textId, , rule, min, max, step] of RULE_FIELDS) {
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
    const label = el("label", "", t(textId));
    label.append(input);
    form.append(label);
  }
  const save = el("button", "wide-field", t("ui-save"));
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
    confirmButton(`remove:${name}`, t("ui-end-connection"), t("ui-confirm-again"), () =>
      call("remove", { name }),
    ),
  );

  box.append(
    form,
    el("p", "note", t("ui-rules-peer-note", { name, rules: rules.peer })),
    reportsForm(contact),
    danger,
  );
  return box;
}

/// Whether this device answers the contact with the reported state, and
/// after how many minutes. Unsaved values are kept in `drafts` like those
/// of the rules.
function reportsForm(contact) {
  const name = contact.name;
  const rules = contact.rules;
  const draft = drafts.get(name) ?? {};
  const form = el("form", "rules");
  form.append(el("h3", "wide-field", t("ui-reports-heading")));

  const allowed = el("input");
  allowed.type = "checkbox";
  allowed.name = "reportsAllowed";
  allowed.checked = draft.reportsAllowed ?? rules.reports_allowed;
  allowed.addEventListener("change", () => {
    drafts.set(name, { ...(drafts.get(name) ?? {}), reportsAllowed: allowed.checked });
  });
  const check = el("label", "wide-field check");
  check.append(allowed, document.createTextNode(` ${t("ui-reports-allowed", { name })}`));

  const delay = el("input");
  delay.type = "number";
  delay.name = "reportsDelay";
  delay.min = 1;
  delay.max = 1440;
  delay.step = 1;
  delay.required = true;
  delay.value = draft.reportsDelay ?? rules.report_delay_minutes;
  delay.addEventListener("input", () => {
    drafts.set(name, { ...(drafts.get(name) ?? {}), reportsDelay: delay.value });
  });
  const delayLabel = el("label", "", t("ui-reports-delay"));
  delayLabel.append(delay);

  const save = el("button", "wide-field", t("ui-save"));
  save.type = "submit";
  form.append(check, delayLabel, save, el("p", "note wide-field", t("ui-reports-note", { name })));
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    save.disabled = true;
    try {
      await call("set_report_setting", {
        name,
        allowed: allowed.checked,
        delayMinutes: Number(delay.value),
      });
      const rest = { ...(drafts.get(name) ?? {}) };
      delete rest.reportsAllowed;
      delete rest.reportsDelay;
      drafts.set(name, rest);
    } catch {
      // Already shown by `call`.
    } finally {
      save.disabled = false;
      await refresh();
    }
  });
  return form;
}

/// The ⚙ button that folds the settings of a contact in and out.
function settingsToggle(name) {
  const toggle = el("button", "icon", "⚙");
  toggle.type = "button";
  toggle.setAttribute("aria-label", t("ui-settings-for", { name }));
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
        el("p", "", t("ui-pairing-accepted", { name })),
        el("p", "verification", grouped(contact.code)),
        el("p", "note", contact.compare_hint),
        row(
          button(t("ui-confirm-code"), () => call("confirm", { name }), "primary"),
          button(t("ui-reject-code"), () => call("reject", { name }), "quiet"),
        ),
      );
      return item;

    case "awaiting_hello_ok":
      item.classList.add("pairing");
      item.append(
        el("h3", "", name),
        el("p", "", t("ui-awaiting-hello-ok", { name })),
        el("p", "verification", grouped(contact.code)),
        el("p", "note", contact.compare_hint),
        row(button(t("ui-reject-code"), () => call("reject", { name }), "quiet")),
      );
      return item;

    case "awaiting_reveal":
      item.classList.add("pairing");
      item.append(
        el("h3", "", name),
        el("p", "", t("ui-awaiting-reveal", { name })),
        row(button(t("ui-cancel"), () => call("reject", { name }), "quiet")),
      );
      return item;

    case "ended":
      item.classList.add("ended");
      item.append(
        el("h3", "", name),
        el("p", "status", t("ui-connection-ended")),
        row(
          confirmButton(`remove:${name}`, t("ui-remove"), t("ui-confirm-again"), () =>
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
      el("p", "", contact.incoming),
      button(t("ui-send-sign"), () => call("respond", { name }), "primary wide"),
    );
  } else {
    item.append(el("h3", "", name));
  }

  if (contact.last_response) {
    const last = el("p", "status", `${t("ui-last-response")} `);
    last.append(el("span", contact.last_response.startsWith("✓") ? "ok" : "", contact.last_response));
    item.append(last);
  }
  if (contact.outgoing) item.append(el("p", "status", contact.outgoing));

  const ask = button(t("ui-ask"), () => call("request", { name }));
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
        t("ui-withdraw"),
        t("ui-confirm-again"),
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
  defaultRelay = setup.default_relay ?? "";
  $("setup").hidden = setup.ready || joining;
  $("join-flow").hidden = !joining;
  $("app").hidden = !setup.ready;
  $("actions").hidden = !setup.ready;
  $("state-path").textContent = t("ui-state-path", { path: setup.path });
  if (!setup.ready) {
    prefillRelay();
    return;
  }
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
  refreshNotifications();
  $("state-banner").hidden = !cards.state;
  $("state-text").textContent = cards.state ?? "";
  $("state-clear").setAttribute("aria-label", t("ui-clear-state"));
  const contacts = [...cards.contacts].sort((a, b) => rank(a) - rank(b));
  $("contacts").replaceChildren(...contacts.map(contactCard));
  $("invitations").replaceChildren(...cards.invitations.map(invitationCard));
  $("invitations-section").hidden = cards.invitations.length === 0;
  $("no-contacts").hidden = cards.contacts.length + cards.invitations.length > 0;
}

let redrawLater = false;

/// The relay address from the build, and whether it is already in the
/// fields (so that typing there is never overwritten).
let defaultRelay = "";
let relayPrefilled = false;

function prefillRelay() {
  if (relayPrefilled || !defaultRelay) return;
  relayPrefilled = true;
  $("setup-form").elements.relay.value = defaultRelay;
  $("setup-join-form").elements.relay.value = defaultRelay;
}

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
function qrSvg(qr, labelId = "ui-qr-invitation") {
  const ns = "http://www.w3.org/2000/svg";
  const side = qr.width + 2 * QR_QUIET_ZONE;
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${side} ${side}`);
  svg.setAttribute("shape-rendering", "crispEdges");
  svg.setAttribute("role", "img");
  // Kept up to date by `applyTexts` when the language changes.
  svg.setAttribute("data-i18n-aria-label", labelId);
  svg.setAttribute("aria-label", t(labelId));
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
  // A pasted invitation of version 1 is refused by the core with the same
  // text as OLD_INVITATION.
  await call("accept", { name, code: data.get("code") });
  result.textContent = t("ui-accepted", { name });
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

/// Which flow of `lz-device` is running in the window: `join` (setup
/// screen), `add` (panel "Devices") or null.
let deviceFlow = null;
let joining = false;

// ---------------------------------------------------------------- Setup screen

const SETUP_WAYS = ["new", "invitation", "code", "join"];

function openSetupWay(name) {
  for (const way of SETUP_WAYS) {
    $(`setup-${way}`).hidden = way !== name;
  }
  for (const toggle of document.querySelectorAll("[data-setup]")) {
    toggle.classList.toggle("primary", toggle.dataset.setup === name);
  }
}

for (const toggle of document.querySelectorAll("[data-setup]")) {
  toggle.addEventListener("click", () => openSetupWay(toggle.dataset.setup));
}

openSetupWay("new");

$("setup-invitation-go").addEventListener("click", async (event) => {
  event.target.disabled = true;
  try {
    await call("init_with_invitation_only");
    await refresh();
    openPanel("accept");
  } catch {
    // Already shown by `call`.
  } finally {
    event.target.disabled = false;
  }
});

$("setup-scan").addEventListener("click", async () => {
  const code = await scanCode("setup");
  if (code) $("setup-code-form").elements.code.value = code;
});

onSubmit("setup-code-form", async (data) => {
  await call("init_from_setup_code", { code: data.get("code") });
});

// ---------------------------------------------------------------- Join flow

function showJoinPart(part) {
  for (const id of ["code", "check", "confirm"]) {
    $(`join-${id}-box`).hidden = id !== part;
  }
}

function resetJoin() {
  joining = false;
  deviceFlow = null;
  showJoinPart(null);
  $("join-progress").textContent = "";
  $("join-failed").hidden = true;
  $("join-cancel").hidden = false;
  $("join-qr").replaceChildren();
  $("join-code").value = "";
}

onSubmit("setup-join-form", async (data) => {
  resetJoin();
  // Events may arrive before the command returns.
  joining = true;
  deviceFlow = "join";
  $("join-progress").textContent = t("ui-moment");
  try {
    await tauri.core.invoke("device_join", {
      relay: data.get("relay"),
      name: data.get("name"),
    });
  } catch (error) {
    resetJoin();
    showError(String(error));
    throw error;
  }
});

$("join-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("join-code").value);
    $("join-progress").textContent = t("ui-copied");
  } catch (error) {
    showError(String(error));
  }
});

$("join-share").addEventListener("click", async () => {
  try {
    await tauri.core.invoke(PLUGIN + "share_text", { text: $("join-code").value });
  } catch (error) {
    showError(String(error));
  }
});

$("join-code").addEventListener("focus", (event) => event.target.select());

async function answerDevice(answer) {
  try {
    await tauri.core.invoke("device_confirm", { answer });
  } catch (error) {
    showError(String(error));
  }
}

$("join-match").addEventListener("click", () => answerDevice(true));
$("join-mismatch").addEventListener("click", () => answerDevice(false));

$("join-cancel").addEventListener("click", async () => {
  await tauri.core.invoke("device_cancel").catch(() => {});
  resetJoin();
  await refresh();
});

$("join-restart").addEventListener("click", async () => {
  await tauri.core.invoke("device_cancel").catch(() => {});
  resetJoin();
  await refresh();
});

function joinEvent({ step, code, text, qr }) {
  switch (step) {
    case "device_code":
      $("join-qr").replaceChildren(qrSvg(qr, "ui-qr-device"));
      $("join-code").value = code;
      showJoinPart("code");
      $("join-progress").textContent = t("ui-join-waiting");
      break;
    case "check_device":
      $("join-check").textContent = grouped(code);
      showJoinPart("check");
      $("join-progress").textContent = "";
      break;
    case "confirm_handover":
      $("join-confirm-code").textContent = grouped(code);
      showJoinPart("confirm");
      $("join-progress").textContent = "";
      break;
    case "progress":
      $("join-progress").textContent = text;
      break;
    case "done":
      resetJoin();
      if (isAndroid) tauri.core.invoke(PLUGIN + "refresh_push").catch((e) => showError(String(e)));
      refresh();
      break;
    case "error":
    case "alarm":
      showJoinPart(null);
      $("join-progress").textContent = "";
      $("join-failed-text").textContent = text;
      $("join-failed").hidden = false;
      $("join-cancel").hidden = true;
      break;
  }
}

// ---------------------------------------------------------------- Devices panel

/// Draws the list of devices of this person.
async function showDevices() {
  let list;
  try {
    list = await call("devices");
  } catch {
    return;
  }
  const form = $("add-device-form");
  const own = list.find((d) => d.this)?.name ?? "";
  // The name field of the add form names this device. Once it has a name,
  // the field goes, so that adding never renames it by accident; renaming
  // is the button in the list.
  $("add-name-row").hidden = own !== "";
  if (own || !form.elements.name.value) form.elements.name.value = own;
  $("devices").replaceChildren(
    ...list.map((device) => {
      const item = el("li", "card");
      const parts = [el("h3", "", device.name)];
      if (device.this) {
        parts.push(el("span", "this-device", t("ui-this-device-tag")));
        parts.push(spacer());
        if (device.name) {
          parts.push(button(t("ui-rename"), async () => renameForm(item, device.name)));
        }
      } else {
        parts.push(
          spacer(),
          confirmButton(
            `device-remove:${device.name}`,
            t("ui-remove"),
            t("ui-confirm-again"),
            async () => {
              await call("device_remove", { reader: device.reader });
              await showDevices();
            },
          ),
        );
      }
      item.append(row(...parts));
      return item;
    }),
  );
  try {
    const code = await tauri.core.invoke("setup_code").then((reply) => reply.value);
    $("setup-code-qr").replaceChildren(qrSvg(code.qr, "ui-qr-setup"));
    $("setup-code-text").value = code.text;
    $("setup-code-details").hidden = false;
  } catch {
    // No access token: nothing to show.
    $("setup-code-details").hidden = true;
    $("setup-code-details").open = false;
  }
}

/// Turns the entry of this device into a field for its new name.
function renameForm(item, name) {
  const form = el("form", "row");
  const input = el("input");
  input.name = "name";
  input.required = true;
  input.maxLength = 32;
  input.value = name;
  input.setAttribute("aria-label", t("ui-rename"));
  const save = el("button", "primary", t("ui-save"));
  save.type = "submit";
  const cancel = el("button", "", t("ui-cancel"));
  cancel.type = "button";
  cancel.addEventListener("click", () => showDevices());
  form.append(input, save, cancel);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    save.disabled = true;
    try {
      await call("device_rename", { name: input.value });
    } catch {
      // Already shown by `call`.
    } finally {
      await showDevices();
    }
  });
  item.replaceChildren(form);
  input.focus();
}

$("setup-code-details").addEventListener("toggle", (event) => {
  // Not left on screen once folded away.
  if (!event.target.open) {
    $("setup-code-qr").replaceChildren();
    $("setup-code-text").value = "";
  } else {
    showDevices();
  }
});

function showAddPart(part) {
  $("add-confirm-box").hidden = part !== "confirm";
  $("add-wait").hidden = part !== "wait";
}

function resetAdd() {
  deviceFlow = null;
  $("add-device-form").hidden = false;
  $("add-device-flow").hidden = true;
  $("add-progress").textContent = "";
  showAddPart(null);
}

$("add-scan").addEventListener("click", async () => {
  const code = await scanCode("device");
  if (code) $("add-device-form").elements.code.value = code;
});

onSubmit("add-device-form", async (data) => {
  $("add-result").hidden = true;
  // Events may arrive before the command returns.
  deviceFlow = "add";
  $("add-device-form").hidden = true;
  $("add-device-flow").hidden = false;
  $("add-progress").textContent = t("ui-moment");
  try {
    await tauri.core.invoke("device_add", {
      code: data.get("code"),
      name: data.get("name"),
    });
  } catch (error) {
    resetAdd();
    showError(String(error));
    throw error;
  }
});

$("add-match").addEventListener("click", () => {
  showAddPart(null);
  answerDevice(true);
});
$("add-mismatch").addEventListener("click", () => {
  showAddPart(null);
  answerDevice(false);
});
$("add-cancel").addEventListener("click", async () => {
  await tauri.core.invoke("device_cancel").catch(() => {});
  resetAdd();
});

function addEvent({ step, code, text }) {
  switch (step) {
    case "confirm_device":
      $("add-confirm-code").textContent = grouped(code);
      showAddPart("confirm");
      $("add-progress").textContent = "";
      break;
    case "check_handover":
      $("add-handover-code").textContent = grouped(code);
      showAddPart("wait");
      break;
    case "progress":
      $("add-progress").textContent = text;
      break;
    case "done":
      resetAdd();
      $("add-device-form").reset();
      $("add-result").textContent = t("ui-add-done");
      $("add-result").hidden = false;
      showDevices();
      break;
    case "error":
    case "alarm":
      resetAdd();
      showError(text);
      break;
  }
}

tauri.event.listen("lz-device", (event) => {
  if (deviceFlow === "join") joinEvent(event.payload);
  else if (deviceFlow === "add") addEvent(event.payload);
});

tauri.event.listen("lz-line", (event) => {
  log(event.payload);
  refresh();
});

// The texts first, so that nothing is drawn in a wrong language; without
// them the English defaults of the page stay.
tauri.core
  .invoke("texts")
  .then(useTexts)
  .catch((error) => showError(String(error)))
  .finally(() => {
    refresh();
    setInterval(refresh, REFRESH_MS);
  });

$("state-clear").addEventListener("click", async () => {
  try {
    await call("clear_state");
  } catch {
    // Already shown by `call`.
  }
  await refresh();
});
