// Foreground window for an open request (X11). The window never takes the
// keyboard focus; a click anywhere answers, keys never do. Closing the
// window answers too; that is handled in src-tauri/src/desktop/alert.rs.
//
// Texts go in through `textContent`, never as HTML.

"use strict";

const tauri = window.__TAURI__;
const $ = (id) => document.getElementById(id);

let sent = false;

tauri.core
  .invoke("alert_text")
  .then((text) => {
    $("title").textContent = text.title;
    $("hint").textContent = text.hint;
  })
  .catch(() => {
    // The default texts stay; the click still answers.
  });

$("answer").addEventListener("click", async () => {
  if (sent) return;
  sent = true;
  try {
    // The app closes this window once the answer is on its way.
    await tauri.core.invoke("alert_answer");
  } catch (error) {
    sent = false;
    $("error").textContent = String(error);
    $("error").hidden = false;
  }
});

// A key press must never count as a sign of life, not even if the window
// should get the focus after all.
for (const type of ["keydown", "keyup", "keypress"]) {
  window.addEventListener(type, (event) => event.preventDefault(), true);
}
