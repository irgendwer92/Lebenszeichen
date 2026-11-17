"use strict";

// Page of lz-server. All URLs are relative (`api/…`): Home Assistant serves
// the page under an ingress path that differs per installation.

const POLL_JOINING_MS = 2_000;
const POLL_MS = 10_000;
const ARM_MS = 4_000;
/// Modules of light margin around a QR code, as in `lz_client::qr`.
const QR_QUIET_ZONE = 4;

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- Texts

let texts = {};

/// The text `id`, with `{ $name }` replaced by `args.name`.
function t(id, args = {}) {
  const text = texts[id] ?? id;
  return text.replace(/\{\s*\$(\w+)\s*\}/g, (whole, name) =>
    name in args ? String(args[name]) : whole,
  );
}

// ---------------------------------------------------------------- API

class ApiError extends Error {}

/// Calls `api/<path>`; returns the parsed JSON or null (204). Shows errors.
async function call(method, path, body) {
  try {
    const options = { method, headers: {} };
    if (body !== undefined) {
      options.headers["Content-Type"] = "application/json";
      options.body = JSON.stringify(body);
    }
    let response;
    try {
      response = await fetch(`api/${path}`, options);
    } catch {
      throw new ApiError(t("web-network-error"));
    }
    const text = await response.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      // Not JSON; handled below.
    }
    if (!response.ok) {
      throw new ApiError(data?.error ?? `${response.status} ${response.statusText}`);
    }
    return data;
  } catch (error) {
    showError(error.message);
    throw error;
  }
}

function showError(message) {
  $("error-text").textContent = message;
  $("error").hidden = false;
}

$("error-close").addEventListener("click", () => {
  $("error").hidden = true;
});

// ---------------------------------------------------------------- DOM helpers

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function button(text, onClick, className = "") {
  const node = el("button", className, text);
  node.type = "button";
  node.addEventListener("click", async () => {
    node.disabled = true;
    try {
      await onClick(node);
    } catch {
      // Already shown by `call`.
    } finally {
      node.disabled = false;
      await refresh(true);
    }
  });
  return node;
}

/// Buttons waiting for their second click, by key, with the time of the
/// first click. Kept outside the buttons because the view is redrawn.
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
        refresh(true);
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
      await refresh(true);
    }
  });
  return node;
}

/// Copies the text of `source` (a textarea or element); selects it when the
/// clipboard is not available (plain HTTP, embedded frame).
async function copyText(value, source, node) {
  try {
    await navigator.clipboard.writeText(value);
    const old = node.textContent;
    node.textContent = t("web-copied");
    setTimeout(() => (node.textContent = old), 1500);
  } catch {
    if (source.select) {
      source.select();
    } else {
      const range = document.createRange();
      range.selectNodeContents(source);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
    }
  }
}

/// Draws a QR code as one SVG path, one unit per module. Built through the
/// DOM, so the Content Security Policy needs no exception for images.
function qrSvg(qr) {
  const ns = "http://www.w3.org/2000/svg";
  const side = qr.width + 2 * QR_QUIET_ZONE;
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${side} ${side}`);
  svg.setAttribute("shape-rendering", "crispEdges");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", t("web-qr-label"));
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

// ---------------------------------------------------------------- Screens

function setupScreen(status) {
  const card = el("section", "card");
  card.append(el("h2", "", t("web-setup-heading")), el("p", "", t("web-setup-intro")));
  const form = el("form", "stack");
  const relay = el("input");
  relay.type = "url";
  relay.name = "relay";
  relay.required = true;
  relay.value = status.default_relay ?? "";
  const name = el("input");
  name.type = "text";
  name.name = "name";
  name.required = true;
  name.value = "Home Assistant";
  const submit = el("button", "primary", t("web-add-device"));
  submit.type = "submit";
  for (const [text, input] of [
    [t("web-relay"), relay],
    [t("web-device-name"), name],
  ]) {
    const label = el("label", "", text);
    label.append(input);
    form.append(label);
  }
  form.append(submit);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    submit.disabled = true;
    try {
      await call("POST", "join", { relay: relay.value.trim(), name: name.value.trim() });
    } catch {
      // Already shown by `call`.
    } finally {
      submit.disabled = false;
      await refresh(true);
    }
  });
  card.append(form);
  return [card];
}

function joiningScreen(status) {
  const join = status.join ?? {};
  const card = el("section", "card");
  card.append(el("h2", "", t("web-join-heading")));
  const step = join.step;
  if (step === "device_code") {
    if (join.qr) {
      const qr = el("div", "qr");
      qr.append(qrSvg(join.qr));
      card.append(qr);
    }
    const code = el("p", "mono code", join.code ?? "");
    code.setAttribute("aria-label", t("web-code-label"));
    card.append(code);
    card.append(el("p", "note", t("web-code-instruction")));
    card.append(button(t("web-copy"), (node) => copyText(join.code ?? "", code, node)));
  } else if (step === "check_device" || step === "check_handover") {
    card.append(el("p", "digits", join.code ?? ""), el("p", "", t("web-check-instruction")));
  } else if (step === "confirm_device" || step === "confirm_handover") {
    const row = el("div", "inline");
    row.append(
      button(
        t("web-matches"),
        () => call("POST", "join/answer", { matches: true }),
        "primary",
      ),
      button(t("web-no-match"), () => call("POST", "join/answer", { matches: false })),
    );
    card.append(
      el("p", "digits", join.code ?? ""),
      el("p", "", t("web-confirm-instruction")),
      row,
    );
  } else if (step === "failed") {
    const banner = el("div", "banner error");
    banner.setAttribute("role", "alert");
    banner.append(el("p", "", `${t("web-join-failed")}${join.error ? `: ${join.error}` : ""}`));
    card.append(banner);
  }
  if (join.progress?.length) {
    const list = el("ul", "note progress");
    for (const line of join.progress) list.append(el("li", "", line));
    card.append(list);
  }
  const row = el("div", "inline");
  if (step === "failed") {
    row.append(button(t("web-start-again"), () => call("POST", "join/cancel"), "primary"));
  } else {
    row.append(button(t("web-cancel"), () => call("POST", "join/cancel")));
  }
  card.append(row);
  return [card];
}

function headerCard(status) {
  const card = el("section", "card");
  card.append(el("h2", "", status.device?.name ?? ""));
  const others = status.device?.others ?? [];
  if (others.length) {
    card.append(el("p", "note", t("web-together-with", { others: others.join(", ") })));
  }
  if (status.state) {
    const banner = el("div", "banner state");
    banner.append(el("p", "", status.state.text));
    const clear = button(
      "✕",
      () => call("DELETE", "state"),
      "icon",
    );
    clear.setAttribute("aria-label", t("web-clear-state"));
    clear.title = t("web-clear-state");
    banner.append(clear);
    card.append(banner);
  } else {
    card.append(el("p", "note", t("web-no-state")));
  }
  return card;
}

function contactRow(contact) {
  const row = el("div", "contact");
  row.append(el("h3", "", contact.name));
  if (contact.incoming) row.append(el("p", "note", contact.incoming));
  if (!contact.paired) {
    row.append(el("p", "note", t("web-not-paired")));
    return row;
  }
  const form = el("form", "inline");
  const check = el("input");
  check.type = "checkbox";
  check.checked = !!contact.reports_allowed;
  const checkLabel = el("label", "check");
  checkLabel.append(check, document.createTextNode(t("web-answer-with-state")));
  const minutes = el("input");
  minutes.type = "number";
  minutes.min = "1";
  minutes.max = "1440";
  minutes.step = "1";
  minutes.required = true;
  minutes.value = contact.report_delay_minutes ?? 15;
  const minutesLabel = el("label", "check");
  minutesLabel.append(
    document.createTextNode(t("web-after")),
    minutes,
    document.createTextNode(t("web-minutes")),
  );
  const save = el("button", "", t("web-save"));
  save.type = "submit";
  form.append(checkLabel, minutesLabel, save);
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    save.disabled = true;
    try {
      await call("PUT", `contacts/${encodeURIComponent(contact.name)}/reports`, {
        allowed: check.checked,
        delay_minutes: Number(minutes.value),
      });
    } catch {
      // Already shown by `call`.
    } finally {
      save.disabled = false;
      await refresh(true);
    }
  });
  row.append(form);
  return row;
}

function contactsCard(status) {
  const card = el("section", "card");
  card.append(el("h2", "", t("web-contacts-heading")));
  if (!status.contacts.length) card.append(el("p", "note", t("web-no-contacts")));
  for (const contact of status.contacts) card.append(contactRow(contact));
  return card;
}

/// The Home Assistant example for the shell commands. The exact YAML
/// templating of the token (`{{ token }}` here) will be finalized during the
/// test with a real Home Assistant; everything about it is in this function.
function exampleYaml(socket) {
  return [
    "shell_command:",
    "  lz_asleep: >-",
    `    curl -sS --fail-with-body --unix-socket ${socket}`,
    '    -H "Authorization: Bearer {{ token }}"',
    '    -H "Content-Type: application/json"',
    `    -d '{"reason":"asleep","minutes":480}' http://lz/state`,
    "  lz_clear: >-",
    `    curl -sS --fail-with-body --unix-socket ${socket}`,
    '    -H "Authorization: Bearer {{ token }}" -X DELETE http://lz/state',
    "",
    "# secrets.yaml:  lz_token: <the token above>",
  ].join("\n");
}

function interfaceCard(status) {
  const card = el("section", "card");
  card.append(el("h2", "", t("web-interface-heading")));
  if (status.http) {
    const warning = el(
      "div",
      "banner warn",
      t("web-http-warning", { address: status.http.address }),
    );
    warning.setAttribute("role", "alert");
    card.append(warning);
  }
  const socketLabel = el("label", "", t("web-socket"));
  const socket = el("input", "mono");
  socket.type = "text";
  socket.readOnly = true;
  socket.value = status.socket ?? "";
  socketLabel.append(socket);

  const tokenLabel = el("label", "", t("web-token"));
  const token = el("textarea", "mono");
  token.readOnly = true;
  token.rows = 2;
  token.value = status.token ?? "";
  tokenLabel.append(token);
  const row = el("div", "inline");
  row.append(
    button(t("web-copy"), (node) => copyText(token.value, token, node)),
    confirmButton("renew", t("web-renew"), t("web-renew-confirm"), () =>
      call("POST", "token/renew"),
    ),
  );

  const example = el("pre", "", exampleYaml(status.socket ?? ""));
  card.append(
    socketLabel,
    tokenLabel,
    row,
    el("h3", "", t("web-example-heading")),
    example,
    el("p", "note", t("web-example-hint")),
  );
  return card;
}

// ---------------------------------------------------------------- Refresh

let lastStatus = "";
let redrawLater = false;
let timer = null;
let loaded = false;

/// An input or text area of the view has the focus: do not redraw it away.
function editing() {
  const active = document.activeElement;
  return (
    (active?.tagName === "INPUT" || active?.tagName === "TEXTAREA") &&
    $("view").contains(active)
  );
}

document.addEventListener("focusout", () => {
  setTimeout(() => {
    if (redrawLater && !editing()) {
      redrawLater = false;
      refresh(true);
    }
  });
});

function draw(status) {
  const view = $("view");
  let nodes;
  if (status.setup === "ready") {
    nodes = [headerCard(status), contactsCard(status), interfaceCard(status)];
  } else if (status.setup === "joining") {
    nodes = joiningScreen(status);
  } else {
    nodes = setupScreen(status);
  }
  view.replaceChildren(...nodes);
}

/// Loads the status and redraws if it changed. `force` redraws even when
/// nothing changed (a pending confirmation, a changed focus).
async function refresh(force = false) {
  clearTimeout(timer);
  let status = null;
  try {
    const response = await fetch("api/status");
    if (!response.ok) throw new Error(String(response.status));
    status = await response.json();
  } catch {
    if (loaded) showError(t("web-network-error"));
  }
  if (status) {
    loaded = true;
    const serialized = JSON.stringify(status);
    if (force || serialized !== lastStatus) {
      if (editing()) {
        redrawLater = true;
      } else {
        lastStatus = serialized;
        draw(status);
      }
    }
  }
  const joining = status?.setup === "joining";
  timer = setTimeout(() => refresh(), joining ? POLL_JOINING_MS : POLL_MS);
}

async function start() {
  try {
    const response = await fetch("api/texts");
    const data = await response.json();
    texts = data.texts ?? {};
    if (data.lang) document.documentElement.lang = data.lang;
  } catch {
    // English ids show until the texts load; the next start retries.
  }
  $("error-close").setAttribute("aria-label", t("web-dismiss"));
  $("error-close").title = t("web-dismiss");
  await refresh(true);
}

start();
