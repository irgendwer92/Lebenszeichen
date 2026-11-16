# M6 – Windows: plan

Done when: M3 and M5 work the same on Windows (roadmap). The work can be
done partly here (code, CI on `windows-latest`) and partly in a Claude Code
session on a Windows PC, which builds and starts the app in the person's
desktop session, so windows, tray and notifications are visible there.

## Starting point

- The desktop code compiles its display of requests only for Linux
  (`#[cfg(target_os = "linux")]` on `desktop/notify.rs` and
  `desktop/alert.rs`). On Windows the app would run with tray and main
  window, but without notification or foreground window.
- Platform-neutral already: tray, closing into the tray, single instance,
  language (`sys-locale`), state file with lock (`File::lock`, works on
  Windows), atomic save by rename.
- `lz`'s default state path (`ctx::default_state_path`) uses only
  `XDG_DATA_HOME` and `HOME`, so on Windows `lz` without `--state` fails.
- No autostart on any platform yet. No installer yet (`bundle.active` is
  false).
- The concept (`docs/concept.md`) sets for Windows: own window above all
  others as the default display, toast as fallback, only
  `Dismissed` with `UserCanceled` counts as an answer, `TimedOut` does not.

## Steps

### w1 – Windows in the CI

- New job "Desktop app (Windows)" in `check.yml` on `windows-latest`:
  `cargo test --workspace`, Clippy and tests for `app/src-tauri`.
- Fix what breaks. Known: `lz`'s default path becomes
  `%APPDATA%\lebenszeichen\device.json` on Windows.
- File permissions: on Unix the state file is 0600; on Windows the file in
  `%APPDATA%` is protected by the user profile's ACL. Noted in the docs, no
  code of our own.

### w2 – Notifications: neutral worker, Windows toast

- `notify.rs` splits into the worker that compares open requests with the
  screen (`Screen`, `update`, `answer`, the tests) and one backend per
  platform: D-Bus on Linux (as today), toast on Windows.
- Toast through the `windows` crate (WinRT `ToastNotification`), the same
  major version as Tauri already pulls in:
  - scenario `reminder` with the button "Send sign of life": stays on screen
    until the person acts, no sound (`<audio silent="true"/>`);
  - `Activated` (click or button) answers; `Dismissed` with `UserCanceled`
    answers; `TimedOut` and `ApplicationHidden` (the app withdrew it) do
    not;
  - the AppUserModelID is the app identifier
    `io.github.irgendwer92.lebenszeichen`; the installer's Start menu
    shortcut carries it. Without installation (development) the app
    registers it under `HKCU\Software\Classes\AppUserModelId\…` so that
    toasts appear. To be checked on the PC.
- Checked on the PC: button, click, closing by hand answer; withdrawal does
  not; toast goes when the request was answered on another device.

### w3 – Foreground window on Windows

- `alert.rs` is reused: always on top, not in the taskbar, never focused
  (`focusable(false)`, on Windows `WS_EX_NOACTIVATE`). "On all workspaces"
  has no effect on Windows; a new window appears on the current virtual
  desktop.
- Do Not Disturb (decision 1): the window only when Windows accepts
  notifications (`SHQueryUserNotificationState` is
  `QUNS_ACCEPTS_NOTIFICATIONS`); during Do Not Disturb, presentation or a
  full-screen game the request comes as a toast, which Windows holds back
  as the person set it.
- Tray: Windows delivers clicks on the icon; a left click shows the window,
  the menu stays on the right click. Linux unchanged.
- Checked on the PC: above all windows, no focus, typing in another program
  sends nothing, click and closing answer, two windows offset, withdrawal.

### w4 – Autostart (Windows and Linux)

- `tauri-plugin-autostart`: registry key `HKCU\…\Run` on Windows, an
  XDG autostart `.desktop` file on Linux.
- Started at login with `--hidden`: the app goes straight into the tray.
- Setting in ⚙ "Start at login"; default (decision 2): turned on at the
  first start, because the app only shows requests while it runs.
- Checked on the PC: log out and in, the tray icon is there, a request
  arrives without opening the window.

### w5 – Installer and release

- Tauri's NSIS installer, per user (`installMode: "currentUser"`, no
  administrator rights), in `tauri.windows.conf.json`; WebView2 through
  the bootstrapper (already present on Windows 10 and 11).
- Workflow "Windows release" on a tag `v*` and by hand, artifact
  `lebenszeichen-windows-setup`, with SHA-256 in the job summary.
- Unsigned for now (decision 3): SmartScreen warns on first start. Signing
  (for example SignPath for open source projects) is an open point for M8.
- Checked on the PC: install, start from the Start menu, uninstall leaves
  no autostart entry behind.

### w6 – Done criterion and docs

- On the PC: Windows app paired with the phone; requests in both
  directions; Windows app as second device of a person next to the Linux
  desktop or the phone (M5): adding with QR/text code and both check codes,
  an answer on one withdraws the request on the other.
- README, roadmap, `concept.md` (Do Not Disturb on Windows) updated.

## Order and split

- w1 first, it shows what the Windows build needs.
- w2 and w3 touch the same files and go one after the other; w4 and w5 are
  independent of them and can run in parallel (subagents).
- Code and CI here; every "checked on the PC" point in the Windows session.

## Decided on 2026-11-16

1. Do Not Disturb: foreground window only when Windows accepts
   notifications, otherwise toast; matches "the system's Do Not Disturb is
   respected".
2. Autostart: on by default after the first start, can be turned off in ⚙.
3. Installer: NSIS per user, unsigned for now.
