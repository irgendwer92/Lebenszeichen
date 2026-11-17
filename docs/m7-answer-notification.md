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
  sound of the system, default expiry, one action (click) that shows the
  main window. Toast: no scenario, no button, default sound, group
  `answers`. Answer notifications never enter `Screen`, so closing or
  clicking one answers nothing.
- Android: channel `answers_v1` (default importance), `AnswerPresenter`.
  In the background `background::fetch` returns `answers` and
  `FetchWorker` shows them; with the app open the core calls the plugin
  command `showAnswer` through the plugin handle.

## Test record

To do on the devices (Phone, Windows, Linux), with requests from the `lz`
test contact:

- [ ] Linux: manual answer and reported answer appear with sound; click
      opens the main window; closing sends nothing.
- [ ] Windows: the same with toasts.
- [ ] Phone, app in the background: notification with sound and name on
      the lock screen; tap opens the app.
- [ ] Phone, app open: the same.
- [ ] Phone: channel "Answers" turned off; answers no longer notify,
      requests still do.
- [ ] One notification per answer on each device.
