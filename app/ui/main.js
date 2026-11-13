// Interface of the M1 prototype. Plain script without a build step; Tauri
// provides `window.__TAURI__` because `withGlobalTauri` is switched on.
"use strict";

const PLUGIN = "plugin:lebenszeichen|";
const REFRESH_MILLISECONDS = 3000;

const byId = (id) => document.getElementById(id);
let lastStatus = null;
let lastLogLines = [];

function call(command, args = {}) {
  const tauri = window.__TAURI__;
  if (!tauri || !tauri.core) {
    return Promise.reject(new Error("Die Tauri-Schnittstelle fehlt. Die Seite läuft nicht in der App."));
  }
  return tauri.core.invoke(PLUGIN + command, args);
}

function showMessage(text) {
  const box = byId("message");
  box.textContent = text;
  box.hidden = !text;
}

async function run(action) {
  try {
    showMessage("");
    await action();
  } catch (error) {
    showMessage(String(error && error.message ? error.message : error));
  }
  await refresh();
}

function setCheck(id, ok) {
  byId(id).textContent = ok ? "Ja" : "Nein";
}

function showStatus(status) {
  lastStatus = status;
  const distributors = status.distributors || [];
  byId("distributor").textContent =
    status.distributor || (distributors.length ? `keiner gewählt (installiert: ${distributors.join(", ")})` : "keiner installiert");
  let registration = "nicht registriert";
  if (status.endpoint) {
    registration = "registriert";
  } else if (status.distributor) {
    registration = status.distributorAcknowledged
      ? "Verteiler hat geantwortet, kein Endpunkt"
      : "wartet auf Antwort des Verteilers";
  }
  byId("registration").textContent = registration;
  byId("endpoint").value = status.endpoint || "";
  byId("register").textContent = status.endpoint ? "Neu registrieren" : "Registrieren";

  const answerUrl = byId("answer-url");
  if (document.activeElement !== answerUrl) {
    answerUrl.value = status.answerUrl || "";
  }

  setCheck("check-notifications", status.notificationsEnabled);
  // Importance 4 is "high"; -1 means the channel does not exist (yet) or Android is older than 8.
  byId("check-channel").textContent =
    status.channelImportance < 0 ? "–" : status.channelImportance >= 4 ? "Ja" : "Nein";
  setCheck("check-fullscreen", status.fullScreenIntentAllowed);
  setCheck("check-battery", status.batteryUnrestricted);

  byId("device").textContent = `${status.device} (API ${status.sdk})`;
  byId("core").textContent = status.core;
}

function showLog(lines) {
  lastLogLines = lines;
  const list = byId("log");
  list.replaceChildren();
  for (const line of [...lines].reverse()) {
    const [time = "", request = "", event = "", detail = ""] = line.split("\t");
    const item = document.createElement("li");
    const head = document.createElement("span");
    head.className = "event";
    head.textContent = event;
    item.append(`${time} `, head, ` ${request === "-" ? "" : request} ${detail}`);
    list.append(item);
  }
  if (!lines.length) {
    const item = document.createElement("li");
    item.textContent = "Noch keine Einträge.";
    list.append(item);
  }
}

async function refresh() {
  try {
    showStatus(await call("status"));
    const log = await call("read_log");
    showLog(log.lines || []);
  } catch (error) {
    showMessage(String(error && error.message ? error.message : error));
  }
}

function curlCommand(endpoint) {
  return `curl -d "wecken" "${endpoint}"`;
}

function requireEndpoint() {
  const endpoint = lastStatus && lastStatus.endpoint;
  if (!endpoint) {
    throw new Error("Es gibt noch keinen Endpunkt. Bitte zuerst registrieren.");
  }
  return endpoint;
}

function bind(id, action) {
  byId(id).addEventListener("click", () => run(action));
}

bind("register", () => call("register", { distributor: "" }));
bind("unregister", () => call("unregister"));
bind("copy-endpoint", () => call("copy_text", { text: requireEndpoint() }));
bind("copy-curl", () => call("copy_text", { text: curlCommand(requireEndpoint()) }));
bind("save-answer-url", () => call("set_answer_url", { url: byId("answer-url").value }));
bind("trigger", () => call("trigger_local", { delaySeconds: 0 }));
bind("trigger-delayed", () => call("trigger_local", { delaySeconds: 10 }));
bind("allow-notifications", () => call("request_notification_permission"));
bind("open-notifications", () => call("open_settings", { target: "notifications" }));
bind("open-fullscreen", () => call("open_settings", { target: "fullscreen" }));
bind("open-battery", () => call("open_settings", { target: "battery" }));
bind("refresh", async () => {});
bind("copy-log", () => call("copy_text", { text: lastLogLines.join("\n") }));
bind("clear-log", () => call("clear_log"));

document.addEventListener("visibilitychange", () => {
  if (!document.hidden) {
    refresh();
  }
});
setInterval(() => {
  if (!document.hidden) {
    refresh();
  }
}, REFRESH_MILLISECONDS);
refresh();
