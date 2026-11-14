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

function log(line) {
  const item = el("li", "", line);
  const list = $("log");
  list.prepend(item);
  while (list.children.length > 50) list.lastChild.remove();
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
    for (const line of reply.lines) log(line);
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
  $("contacts").replaceChildren(...cards.contacts.map(contactCard));
  $("invitations").replaceChildren(...cards.invitations.map(invitationCard));
  $("no-contacts").hidden = cards.contacts.length + cards.invitations.length > 0;
}

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

onSubmit("invite-form", async (data, form) => {
  const code = await call("invite", {
    name: data.get("name"),
    voucher: data.get("voucher") === "on",
  });
  $("invite-code").value = code;
  $("invite-result").hidden = false;
  form.reset();
});

onSubmit("accept-form", async (data, form) => {
  await call("accept", { name: data.get("name"), code: data.get("code") });
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
