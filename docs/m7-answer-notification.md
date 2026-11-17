# M7 – Answer notification

Done when: on the phone and on the desktop, an answer to an own request
appears as a notification, and the channel on Android can be turned off on
its own (roadmap).

## Decisions

1. Answer notifications have sound, on the desktop and on Android.
2. On Android one notification per contact; a newer answer replaces the
   older one.
3. On the Android lock screen the notification shows the name.

## How it works

- `lz-client`: `Ctx::on_notice` is called with every notice as it happens.
  The core drops an answer that arrives again (`AlreadyAnswered`), so each
  answer comes once per own device.
- `app/src-tauri/src/answer.rs` turns `Notice::Response` into the text
  ("Anna has answered", then the time or "“asleep” (reported)") and hands
  it to the platform. The hook is set on every fetch path: the live
  connection (`spawn_listener`), "Fetch" in the tray (`spawn_sync`) and the
  `sync` command.
- Desktop (`desktop/notify.rs`): `Kind::Answer`. D-Bus: urgency normal,
  the sound `message-new-instant` as `sound-name` hint (without a hint the
  services play nothing), default expiry, one action (click) that shows
  the main window, from the main thread. Toast: no scenario, no button,
  default sound, group `answers`, protocol activation of
  `lebenszeichen-open:`; the app registers that scheme under
  `HKCU\Software\Classes` at every start (the uninstaller leaves it, like
  the AppUserModelID). Answer notifications never enter `Screen`, so
  closing or clicking one answers nothing.
- Android: channel `answers_v1` (default importance), `AnswerPresenter`.
  In the background `background::fetch` returns `answers` and
  `FetchWorker` shows them; with the app open the core calls the plugin
  command `showAnswer` through the plugin handle.

## Test record (2026-11-17)

Requests from the user's devices to an `lz` test contact, which answered
with `lz respond` and `lz report … asleep`.

- [x] Linux (KDE Plasma 6.7, Wayland): manual and reported answer, text
      and sound; click opens the main window from the tray; closing sends
      nothing. Plasma plays a sound only with the `sound-name` hint, and
      not while the system's "Notification" stream is muted.
- [x] Windows: toast with sound, no button. A click on a toast in the
      notification center does not reach a running app that is not
      packaged, so the answer toast opens `lebenszeichen-open:`, a scheme
      the app registers for the user at every start; the single-instance
      check shows the window. Windows did not ask which app to use.
- [x] Phone (Fairphone 4, signed release APK from the branch), app in the
      background and locked: notification with sound and name on the lock
      screen; tap opens the app. App open: shown through `showAnswer`.
- [x] Phone: channel "Answers" off; no notification for an answer, a
      request still arrives on the channel of requests.
- [x] Each answer notified once on each device (Linux, Windows and phone
      at the same time).
