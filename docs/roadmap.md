# Lebenszeichen – Roadmap

Status: 2026-11-13. Belongs to `concept.md` and `protocol.md`. The order of the milestones is confirmed.

## Repo structure

One repository for everything. Core, relay and app often change together at first, and this way every protocol change stays a single commit.

```
lebenszeichen/
├── crates/
│   ├── wire/        Message envelope and relay interface (used by core and relay)
│   ├── core/        Protocol, encryption, rule checking, storage
│   ├── cli/         Client without a user interface, can also be used as a service
│   └── relay/       Relay server
├── app/
│   ├── src-tauri/   Tauri shell: tray, window, bridge to the core
│   ├── ui/          User interface (web)
│   └── android/     Kotlin layer: UnifiedPush, full-screen display, notifications
├── deploy/          docker-compose for relay and ntfy, example for the reverse proxy
├── docs/            Concept, protocol, guide for self-hosters
├── examples/        Example scripts for the local interface
├── .github/workflows/
├── renovate.json
├── LICENSE          GPL-3.0-or-later with app store exception
└── crates/relay/LICENSE   AGPL-3.0-or-later
```

Notes:

- **`cli` is not a by-product.** It makes it possible to test the protocol before there is a user interface. Later it is the client for devices without a screen, for example a server on which a script reports the state "asleep".
- **Code, comments, documentation and commit messages in English** (since 2026-11-16; before that, documentation and commit messages were German). The message types from `protocol.md` are called `request`, `response`, `rules`, `hello`, `hello_ok`, `end` in the code. This makes a later publication easier.
- **Licenses:** `wire`, `core`, `cli` and `app` are under GPL-3.0-or-later with an exception, `relay` under AGPL-3.0-or-later. Both licenses explicitly allow being combined with each other.

## CI and dependencies

| Workflow | Trigger | Contents |
|---|---|---|
| Check | every push and pull request | Formatting, Clippy, tests, license and security check of the dependencies (`cargo-deny`) |
| Build | every push to `main` | Linux package, Windows installer, Android APK, relay image; available as artifacts |
| Publish | version tag | Signed APK, installer and relay image as a release |
| Renovate | weekly | Combined pull request for small updates; security updates immediately and individually |

The signing key for Android is stored as a secret in the repository and is backed up externally. If it is lost, installed apps can no longer be updated.

## Milestones

Each milestone ends with something that can be tried out.

### M0 – Foundation (done on 2026-11-13)
- Repository, workspace, license files, CI workflow "Check" (then named "Prüfen"), Renovate configuration.
- **Done when:** a commit passes the CI with green status. Met.
- Still open from M0: install the Renovate app, have the wording of the app store exception reviewed legally (draft in `license-exception.md`).

### M1 – Walking skeleton on Android (completed on 2026-11-14)
Throwaway prototype that only clears up the risk. Result and details are in `m1-walking-skeleton.md`: All ten runs passed on a Fairphone 4, the decision for Tauri stands. The overnight run (rows 9 and 10 of the test log) was made up in the night of 2026-11-13 to 2026-11-14 according to the log and passed.
- The wake signal via UnifiedPush arrives while the app is closed.
- The Kotlin layer calls a function in the Rust core.
- The request appears in the foreground; tapping and dismissing each trigger one call to a test server.
- **Done when:** this works on a real device ten times in a row, also after a restart and overnight.
- **Abort criterion:** If this does not work reliably with Tauri, we switch to Flutter here. Core and relay are not affected by this.

### M2 – Core and relay (completed on 2026-11-14)

Status:

- Done: wire format in `crates/wire` without encryption (plaintext of all six message types, invitation as `LZ1-` code, envelope check).
- Done: encryption and pairing in `crates/core` (both envelope forms checked byte by byte against libsodium, verification code, flow from invitation to `hello_ok`, time window for replay protection).
- Done: rules for requests in `crates/core` (open request, minimum interval, sliding weekly limit, with the same check on both sides), responses, reported responses, removal, detection of repeated messages.
- Done: storage in `crates/core` as one JSON file per device, with lock and atomic replacement.
- Done: relay `lz-relay` (axum, SQLite) with all calls from `wire-format.md`, delivery via WebSocket with acknowledgments, wake signal with protection against private target addresses, cleanup rules and Docker image (`crates/relay/Dockerfile`); integration tests run without a network.
- Done: command line `lz` in `crates/cli` (invite with and without voucher, accept, confirm, fetch via WebSocket with acknowledgment after saving, requests, responses, report, rules, removal including detection via `unknown_mailbox`), tested against an in-memory relay.

Scope:

- Message envelope, encryption, pairing, request and response, rules including weekly limit, removal.
- Relay with mailboxes, readers, vouchers, cleanup rules; as a Docker image.
- Multiple readers per mailbox are planned from the start.
- Requirements so that microcontroller clients remain possible later (see below): language-neutral, documented wire format; protocol part of the core without dependency on the operating system.
- **Done when:** two `cli` instances pair via a relay, send requests and responses, and the rules take effect.

### M3 – Linux desktop (started on 2026-11-14)

Status:

- Done: shared client layer `crates/client` (`lz-client`): relay access, all commands, fetch with `sync` and `listen`, German texts and a structured overview of the contacts. `lz` is now only the command line on top of it; the desktop app uses the same layer.
- Done: desktop shell for Linux in `app/` (`ui/desktop.html`, `src-tauri/src/desktop.rs`): set up device, contact list with request button and "request possible again from", invite (code as text), accept, confirm or reject verification code, respond, remove; fetch in the background via `lz-client`. The Android parts from M1 are unchanged. On 2026-11-14 clicked through against a local relay and two `lz` devices; the errors found in the process are fixed.
- Done: Shared texts in `lz-client` no longer name commands. Where a next step fits, there is a `Hint`; `lz` turns it into its command, the desktop app shows nothing because it has buttons.
- Done: The CI builds the desktop app (formatting, Clippy, tests) and checks licenses and sources of its own lockfile with the same `deny.toml`. Decided: The desktop app is its own device with its own state in the app's data directory, not the same file as `lz`.
- Done: Tray with "Fenster zeigen", "Jetzt abrufen" and "Beenden". Closing only hides the window, the fetch keeps running. A second start brings back the window of the running app instead of fetching a second time; this way the app stays reachable even without a tray. On Linux the tray needs `libayatana-appindicator` or `libappindicator` at runtime (note for the Linux package in M8).
- Done: Notification on Linux via D-Bus (`src-tauri/src/desktop/notify.rs`): urgency "critical", without sound, stays until answered, with button "Send sign of life". Button, click and closing by hand send the sign of life; expiry and withdrawal by the app do not. If the request is no longer open, for example because it was answered in the window, the app withdraws the notification. If sending fails, it comes back. Checked on KDE Plasma (Wayland) against a local relay.
- Done: Strict Content Security Policy for the app (only own files and Tauri's IPC, no inline code) instead of `"csp": null` from M1. The desktop interface runs with it; the Android prototype uses the same configuration and will be checked in M4.
- Done: Invitation as QR code (`crates/client/src/qr.rs`, error correction M, alphanumeric mode; version 8 to 12). The desktop app shows it black on white above the text code, as SVG from the DOM, so the CSP stays unchanged; `lz invite --qr` shows it in the terminal. Scanned with a QR app on the phone, the text matches. Scanning in the app comes with M4.
- Done: Settings per contact in the desktop app: how often the contact may ask (minimum interval and expiry in half hours up to one week, weekly limit 0 to 999), plus a read-only display of how often you may ask yourself. Only changed values are sent. Decided: The switch for reported responses comes with M8, together with the automation that uses it.
- Part of the done criterion met on 2026-11-14: desktop app and `lz` have requested and answered each other, against a local relay with freshly paired devices. The app asks via the button and receives the response in the background; `lz` asks, the app shows the request as a notification and in the window, the response arrives at `lz`. The lockouts afterwards follow the rules of both sides.
- Done: Desktop interface reworked, with its own stylesheet (`style.css` stays with the Android prototype): contact list as main view, open requests highlighted at the top, verification codes large, settings and "Verbindung beenden" behind ⚙, invite and accept as fields from the header, history collapsed. Light and dark following the system, green accent. With this, the "interface reworked" part of the done criterion is met.
- Done: Foreground window on X11 (`src-tauri/src/desktop/alert.rs`, `ui/alert.html`): instead of the notification, a small window above all others, without keyboard focus, not in the taskbar, one per request and slightly offset. Clicking into the window or closing it sends the sign of life, keys never do. If the request was answered another way, the app closes the window without a response; if it cannot be opened, the notification appears. The window may only fetch the text and response for its own request (own capability). Checked under XWayland on KDE Plasma (`GDK_BACKEND=x11`): above all windows, no focus, typing in another program sends nothing, click, closing, withdrawal and two windows at the same time. "On all workspaces" could not be checked with only one workspace.
- Still open in M3: check the notification on GNOME (so far only KDE Plasma), check the foreground window in a real X11 session, also with multiple workspaces.

Scope:

- Interface: contact list, request button, invitation as QR and text code, settings per contact.
- Tray, persistent connection, foreground window without keyboard focus, notification as fallback.
- Open from a Dependabot report of 2026-11-14: Tauri 2 needs the GTK 3 bindings on Linux and therefore `glib` 0.18; the bug in `VariantStrIter` is only fixed from 0.20, which exists only for GTK 4. The affected function is called neither by Tauri nor by GTK, WebKit, `wry` or `tao`, and the Android build does not contain `glib` at all. Check again with the Linux build whether Tauri has meanwhile moved to newer bindings.
- **Done when:** desktop app and `cli` can request each other,
  the request appears on X11 as a foreground window without keyboard focus,
  the notification is checked on KDE Plasma and GNOME and the
  desktop interface is reworked.

### M4 – Android app (completed on 2026-11-15)
- The prototype from M1 is replaced by the proper connection to the core.
- Scan QR code, full-screen display, notification as fallback.
- Signed APK from the CI. Workflow `android-release.yml` built (tag `v*` or by hand, never on pull requests; environment `release`; signing with `apksigner` outside of Gradle; R8 rule for `rustls-platform-verifier`; draft release for tags). Open: create and store the key, see `android-release.md`, then the first run.
- Done: The "Android debug" workflow (then "Android-Prototyp") runs automatically again on pull requests touching `app/**`, `crates/**` and `Cargo.lock`. The first run on 2026-11-14 with the dependencies from M2 passed. So far it contains only `lz-core`; `lz-client` comes into the Android build with the shared connection.
- Done: Push address at the relay. The device stores it (`push_url`) and registers it with each of its own mailboxes, also with new ones from invite and accept. If that fails, it stays pending (`push_pending`), and the next fetch registers it later. An unchanged address costs no call, because UnifiedPush reports it again at every start. `lz push` sets and removes it by hand.
- Done: TLS certificates are checked by the operating system (`rustls-platform-verifier` 0.6, because 0.7 would need `jni` 0.22 and Tauri still uses 0.21), the same for HTTP and WebSocket. On Linux nothing changes with this; on Android the client uses the system's check with self-installed certificates. There it needs a one-time call with the app context and the Kotlin library of the crate (see below). Error messages now name the reason, for example an unknown certificate.
- Done: Shared connection for desktop and Android. The commands of the interface are in `src-tauri/src/commands.rs` and run on both systems via `lz-client`, as does the fetch in the background while the app is open. `desktop.rs` keeps tray, notification and foreground window. On Android the state is in `device.json` in the app's data directory.
- Done: The certificate check is set up on Android at startup, with the app context that Tauri (Tao) stores in `ndk-context`, and before the first connection is made. The Kotlin library of `rustls-platform-verifier` comes from the Maven folder of the crate `rustls-platform-verifier-android`; `build.gradle.kts` finds it via `cargo metadata` with the version from `Cargo.lock`. It contains no ProGuard rules; for a shrunk release build it needs its own.
- Done: One interface for both systems (`ui/index.html`, `app.css`, `app.js`); the files of the M1 prototype are removed. For the phone: spacing to the status bar and gesture bar, larger buttons on touch, 16 px in fields against zooming. Only on Android, ⚙ shows "This device": registration for wake signals, notifications, channel, full screen and battery optimization, each with a button to the settings.
- Done: Wake signal, fetch, display and response run via the core instead of the prototype. A wake signal starts a fetch (`FetchWorker`, WorkManager, also in idle state with network); further wake signals in the meantime append a fetch. For each contact with an open request one notification "Anna is asking for a sign of life" with full-screen display; tapping, swiping away and the reactions of the full-screen display from M1 send the response encrypted via `AnswerWorker`. What is answered, expired or done in the app is withdrawn by the app; for expiry it schedules its own check (`ExpiryWorker`). If a response fails for good, the notification appears again. On the lock screen only "Lebenszeichen?" is shown, the name of the contact only after unlocking (decided on 2026-11-14). The full-screen display observes this itself; for the notification Android decides, and only if the lock screen does not show confidential content. Unlocking from the full-screen display counts as a response (`fullscreen-leave`, as in M1), even if the name was not visible; every reaction is a sign of life (decided on 2026-11-14 after the first test on the phone). A new endpoint from UnifiedPush goes via `PushUrlWorker` to the core and from there to each mailbox. Tauri and the background use the same state file (`device.json` in `getFilesDir()`); a device from the state before this step moves there once. The live connection of the window exists only while the app is in the foreground (`onPause`/`onResume`); otherwise the frozen process would keep it open, and the relay would not wake the phone (noticed in the first test on the phone). The test aids from M1 (response address, local triggering, `prototype::marker`) are removed; the log of the Kotlin layer is under "This device".
- Done: Scan invitation via QR code (Android only). "QR-Code scannen" in the field "Einladung annehmen" opens the camera (CameraX); every image is examined in memory only, `lz_client::qr::decode_invitation` (`rqrr`) looks in it for a code with `LZ1-` and ignores other QR codes. The detected code is then in the field. `rqrr` runs optimized even in the debug build. Without camera permission, pasting remains.
- Done on 2026-11-14: Relay and ntfy run on the own server behind NetBird's Traefik, at `relay.chaos4all.de` and `ntfy.chaos4all.de`, both without an access log. ntfy lets everyone write only to `up*` and only the phone's account read. Pairing, request, response and wake signal checked from outside. The relay wakes any public distributor; `LZ_PUSH_ALLOWED_HOSTS` stays off for now.
- Done criterion met on 2026-11-14, on a Fairphone 4 with the debug APK against `relay.chaos4all.de` and ntfy on the own server: invitation scanned via QR code, paired, then requested and answered in both directions, with `lz` and with the desktop app. Checked: wake signal with the app closed, full-screen display with a locked screen (unlocking counts as a response), banner with the screen on and swiping away as a response, response in the app, checkmark on the phone after the other side's response. Found and fixed in the process: The live connection of the frozen process prevented the wake signal.
- Done on 2026-11-15: first signed release APK from the CI (version 0.1.0, 18.5 MB instead of 46 MB as a debug build; signature schemes v2 and v3). The certificate's fingerprint matches the key, all checks of the APK also pass after R8. The secrets are only in the environment `release`, which only `main` and tags `v*` may use.
- Fixed after the first release build on the phone (2026-11-15): R8 had removed the constructor of the WorkManager database (crash at startup; rule in `consumer-rules.pro`). The release build must not allow http, unlike the debug build; but Android downloads the revocation list (CRL) from Let's Encrypt via http, and the certificate check reported every certificate as "widerrufen". Now `network_security_config.xml` allows http for the Java side; the connection to the relay runs in Rust and is not affected by it. In addition, the app excludes backup and transfer to a new device (`allowBackup`, `dataExtractionRules`), because the state file contains the private keys.
- Release app checked on the phone on 2026-11-15: setup, pairing via QR code with the desktop app, request in both directions, wake signal with the app closed and the screen locked. Since 2026-11-16 the alias of the key is stored as a variable instead of a secret, so that the logs stay readable.
- **Done when:** the phone and desktop of two people can request each other. From here on the app is usable in everyday life, with one device per person.

### M5 – Multiple devices (completed on 2026-11-16)
- Prepared on 2026-11-16: all documentation, CI and configuration comments translated to English; the German documents as of the end of M4 are kept only locally (`docs-de_after-m4/`, ignored by git). Commit messages are English from now on. Planned after M5: the interface in English (default) and German, following the system language and selectable in the settings. Further languages later (decided on 2026-11-16); the texts therefore go into one file per language.
- Set up a second device via QR code, sync of contacts, counters and completed requests.
- Remove a device.
- Planned for the interface of M5 (decided on 2026-11-16): the new device only shows the device check code, the confirmation happens on the existing device. The setup screen gets two more ways besides "new account": "I have an invitation" (no relay address, no access token; the mailbox comes with the invitation's voucher) and a setup code that a holder of the access token shows as a QR code (relay address and access token, so nothing has to be typed; only shown directly, never sent). The relay address in the setup is prefilled from a build-time variable `LZ_DEFAULT_RELAY`, filled by CI from a GitHub variable, so the address is not in the code.
- Found in the device test of 2026-11-16, to fix before M5 is closed: the full-screen display stayed open when the request was answered on another device (only the notification was withdrawn; the same applied to expiry since M4), fixed by closing withdrawn displays. Open for the interface: saving rules gives no confirmation; the name field when adding a device should say clearly that it names the existing device; devices cannot be renamed yet. When the first device of a person deletes a shared mailbox, a second device still reports `unknown_mailbox` for it; that message should stay silent. Also fixed: removing a device went by name, so with two devices of the same name the phone refused to remove the other one as "this device"; the app now removes by reader identifier, and a new device whose name is taken gets a number ("Laptop 2").
- Done on 2026-11-17, after the test on Windows confirmed the trap (the phone was renamed twice to the name meant for the new device): the join screen asks for the "name of this new device", the add screen for the "name of the device you are using now", and only as long as that device has no name; once it has one, adding never renames it. "Rename" in the device list and `lz device rename` rename this device, and the other own devices learn the new name.
- Done criterion met on 2026-11-16, with the release app on a Fairphone 4, the desktop app and `lz` against `relay.chaos4all.de`: the desktop added as another device of the phone (device code scanned, both check codes compared); a contact paired on the desktop arrived on the phone; a request answered on one device was withdrawn on the other, with the phone locked and the app closed (notification and full-screen display); the weekly limit counted across both devices (a third request despite the limit was discarded by both); a request sent from the desktop was answered and shown on the phone; rules changed on the desktop reached the contact and the phone; the desktop removed from the phone returned to the setup screen; a device set up with a setup code; a device set up with "I have an invitation" (no relay account, no token) that then added a second device through a derived mailbox. Three bugs found and fixed on the way: the full-screen display stayed open when another device answered; removing went by name and failed with two devices of the same name; the existing device did not show the handover check code.
- Done on 2026-11-16, tested on the phone with the release app: invitation version 2 (`LZ2-`) with commitment and `reveal`; the accepting side shows "Waiting for the check code …" and then the same code as the inviter, with the hint to compare it in person or on the phone; an old `LZ1-` invitation is refused. The security item it fixes, from the crypto review of 2026-11-16: the verification code of the pairing (M2) can be matched by someone who controls the channel an invitation is sent through (a messenger, for example), by searching about a million keys. The fix needs a commitment before the keys are revealed and changes the invitation format. Until then the app should advise passing invitations on in person or by scanning.
- Done on 2026-11-16: the interface in English (default) and German. All texts of `lz`, the app, the desktop notifications and the Android parts come from one Fluent file per language (`locales/`); the Android-native texts are in `strings.xml`. ⚙ → Language: System, English or Deutsch, per device. Checked on the phone with the release app: the app texts in both languages and Android's per-app language as "System". Still to check on the phone: notification, full-screen display and lock screen in both languages.
- **Done when:** a response on the phone withdraws the display on the computer and the weekly limit counts across both devices.

### M6 – Windows
- Installer, tray, foreground window, autostart.
- Plan, decisions and test record in `m6-windows.md`.
- Done on 2026-11-17, checked on a Windows 10 PC with the installer from the CI ("Windows release"): installer per user, tray, foreground window without keyboard focus together with a notification that stays, Focus Assist respected, start at login into the tray (also on Linux), the Windows app as second device of a person. The relay now accepts 60 messages per hour and mailbox.
- Checked on Linux the same day: notifications, start with the window, "Start at login" and tray work as before.
- Still open in M6: Do Not Disturb on Windows 11.
- **Done when:** M3 and M5 work the same on Windows.

### M7 – Answer notification (completed on 2026-11-17)
- Feedback from a tester: the requester learns of the answer only in the app. A notification when the answer arrives: "Anna has answered" with the time, for a reported answer with the reason ("“asleep” (reported)").
- Android: notification on its own channel, which can be turned off in the system settings without touching the channel of requests. Desktop: an ordinary notification that expires on its own; closing it sends nothing.
- Wording says that an answer came, not that the person is fine (see `concept.md`).
- One notification per answer, also when another device of the same person answered.
- Plan, decisions and test record in `m7-answer-notification.md`. Decided on 2026-11-17: answer notifications have sound on all systems, the name shows on the Android lock screen, and a newer answer of a contact replaces the older notification on Android.
- Done criterion met on 2026-11-17 on Linux (KDE Plasma), Windows 10 and a Fairphone 4 with the release APK: manual and reported answers notify with sound on each device once; a click opens the app; the channel "Answers" turned off on the phone silences answers while requests still arrive. Found on the way: Plasma plays a sound only with a `sound-name` hint; on Windows a click in the notification center reaches the app only through a link scheme (`lebenszeichen-open:`).
- **Done when:** on the phone and on the desktop, an answer to an own request appears as a notification, and the channel on Android can be turned off on its own.

### M8 – Local interface
- `set` and `clear` via Unix socket, named pipe and Android intent with token.
- Reported response after the waiting time, allowed per contact and device.
- Example script.
- Plan, decisions and test record in `m8-local-interface.md`.
- **Done when:** a script reports "asleep" and the requester sees "“asleep” (reported)" after the waiting time.

### M8b – Container and Home Assistant
- Docker image of `lz listen` (amd64, aarch64 later), state in a volume, socket in a mounted directory.
- Home Assistant add-on on the same image; socket in `/share/lebenszeichen/`, with token; `shell_command` with `curl --unix-socket`.
- Optional in both: an HTTP port for the same interface, off by default, only with a token, with a warning that plain HTTP exposes the token on the network.
- **Done when:** a Home Assistant automation and a host script each report "asleep" and the requester sees "“asleep” (reported)".

### M9 – Hardening
- Review of encryption and relay by a second person.
- Guide for self-hosters, including logs turned off in the reverse proxy.
  - Note in it: The relay sends wake signals only to public addresses. ntfy has to be reachable from the internet anyway, otherwise the phone gets no wake signals when on the road. If relay and ntfy run on the same home server and the router cannot route connections to its own public address back (NAT loopback), a local DNS entry for the private address does not help, because the relay rejects it. The solution lies in the own network, for example turning on NAT loopback in the router.
- Test behavior on relay outage, clock deviation and lost device.
- **Done when:** someone without prior knowledge can set up a relay and pair two devices using the guide.

### Later
iOS and macOS, publication in stores, third-party relays, microcontroller clients.

## Microcontroller clients (later)

Wanted, but without a date and without a decision between the approaches. The idea is, for example, a button with a light that shows an open request and answers it when pressed. Both approaches have their place: approach A for at home, approach B for on the road, for example when the smartphone is forgotten.

**Approach A – satellite of a full client.** The microcontroller talks in the home network to a client that is already running (for example `cli` on a server), via a bridge script or home automation. Keys stay on the client.
- Prerequisite: The local interface gets two more calls, "open request present?" and "answer". Not yet decided.
- Low effort, possible from M8.
- Works only as long as the client is reachable at home.

**Approach B – standalone client.** The microcontroller speaks the protocol itself with the relay.
- Feasible on devices of the ESP32 class: WiFi, TLS, WebSocket and libsodium are available there, the messages are 256 bytes in size.
- No wake signal needed; the device keeps the connection itself.
- Needs a time from the network (NTP), because messages carry timestamps.
- The microcontroller would be another device of the person and, according to the current draft, would hold all contact keys. Without encrypted storage this is a risk in case of theft. A remedy would be a restricted device role; the protocol does not have one yet.

What is already being done for this now, without extra effort:
- Wire format language-neutral and fully documented, not tied to a Rust library.
- Relay interface stays plain HTTPS and WebSocket.
- Protocol part of the core separated from storage and operating system.

## Order and dependencies

- M1 deliberately comes before M2, although M2 is the foundation: M1 is the only place where the framework decision can still flip.
- M2 and M1 can be worked on in parallel, because M1 only needs a placeholder function in the core.
- M5 comes after M4, but the relay and the core know multiple devices already from M2. This way nothing has to be changed in the protocol later.
- M6, M7 and M8 are independent of each other and can be swapped.

## Open points

- Wording of the app store exception.
- Protection of the keys on the device. The storage format is decided, see `concept.md`.
- Restricted device role for devices without secure storage.
