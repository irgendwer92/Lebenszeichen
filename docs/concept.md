# Lebenszeichen – Concept

As of 2026-11-13. Technical details on pairing, messages and the relay are in `protocol.md`; repo structure and milestones are in `roadmap.md`.

## Idea

One person asks another for a minimal sign of life. Responding takes practically no effort and reveals nothing except "someone reacted on the device". No chat, no online status, no schedule, no alarm chain.

## Decisions

| Topic | Decision |
|---|---|
| Response options | Exactly one. There is nothing to choose. |
| What counts as a response | Any reaction to the display: tapping, clicking, opening or dismissing. |
| Display of the request | Default: pushes itself into the foreground like an alarm clock. Fallback: notification. Configurable per device. |
| Sound and vibration | Off by default. The person being asked can turn them on on their device. |
| Automatic response from device usage | No. It says too little. |
| Status report from outside | Yes, via a small local interface (reason + time span). Takes effect only after the waiting time for a normal response has passed. |
| Waiting time before a reported response | Configurable, default 15 minutes. |
| Loadable plugins in the app | No. The local interface is enough. |
| Rules for requests | Per contact: at most one open request, expiry after 24 hours, 6 hours minimum interval after a response, at most 7 requests per week. The person being asked sets expiry, interval and weekly limit. |
| Multiple devices | From the start: phone and computer share keys and mailboxes. |
| Removing a contact | Visible to the other side. |
| Forward secrecy | Deliberately waived in favor of a small protocol that supports multiple devices. |
| Client storage | One JSON file per device behind an interface in the core, replaced as a whole on every change. Exactly one process owns a device's state. Keys unencrypted for now, readable only by the owner; protection via the system keystore follows with the apps. Decided on 2026-11-14. |
| UI technology | Plain HTML, CSS and JavaScript in `app/ui`, with no build step and no framework. Sufficient for the contact list, the request button and settings, and brings no npm dependencies. Decided on 2026-11-14. |
| Framework | Tauri, with a standalone Rust core and a thin Kotlin layer on Android. |
| License | Clients and core: GPL-3.0-or-later with app store exception. Relay: AGPL-3.0-or-later. |
| Source code hosting | GitHub, with GitHub Actions and Renovate. |
| Order of milestones | As in `roadmap.md`, confirmed. |
| Operation | Self-hosted at first, apps via sideloading. |
| Publication | Possibly later; the architecture should not rule it out. |
| Platforms | Android, Windows, Linux first; iOS and macOS possibly later. |

## Flow

1. A taps "Lebenszeichen?" on contact B.
2. On B's devices the request appears in the foreground where the system allows it, otherwise as a notification. By default without sound and vibration.
3. B taps it or closes it. Both send the response.
4. A sees "✓" with the time.

If B does not react within the waiting time:
- If a valid status report exists, the app sends it as the response. A sees the reason and that it was reported, not tapped, e.g. "“asleep” (reported)".
- If none exists, nothing happens: no reminder, no "ignored" display for B, and for A the request stays open and expires.

The display stays in place after a reported response. If B reacts later after all, A additionally gets the normal "✓".

## Principles

- The app must not become a surveillance tool. The person being asked determines how often they may be asked, and their device enforces it.
- How a request makes itself noticed is decided solely by the person being asked, on their device. The requester can influence neither sound nor urgency.
- No history statistics, no streaks, no read receipt, no delivery receipt.
- The app is not an emergency call system. No response does not mean "emergency".
- A "✓" means "someone reacted on the device", not "the person is fine".
- Reported responses are always recognizable as such for A.

## Display of the request

The wake signal from the relay to the device is invisible. Only what the app displays in response is visible.

### Foreground (default)

| Platform | Implementation | Limitation |
|---|---|---|
| Android | Notification with full-screen intent, like alarm clock and phone call | Full screen only when the screen is locked or off; during use a banner appears. The permission is active when sideloaded; when installed via the Play Store, users would have to enable it themselves. |
| Windows | Notification that stays until acted on, and in addition an own window above all others | The window only while Focus Assist (Do Not Disturb), presentations and full-screen programs are off. Focus Assist has no documented interface; the app reads it through an undocumented one and shows the window if that fails. |
| Linux, X11 | Own window above all others | – |
| Linux, Wayland | Not reliable: apps may not bring themselves to the front | Fallback to a notification with urgency "critical" that stays visible. On KDE Plasma, a KWin window rule helps locally. |
| macOS (later) | Own window | – |
| iOS (later) | Not possible | Notification only. |

Rules for the foreground display:
- The system's "Do Not Disturb" is respected. Exception: on Linux the
  notification comes with urgency "critical", and KDE Plasma by default shows
  such notifications even during "Do Not Disturb". Anyone who does not want
  that turns it off in the system settings. The foreground window on
  X11 also always appears, because "Do Not Disturb" cannot be queried
  consistently there. On Windows the notification always comes and Windows
  holds it back as set; the window appears only while Focus Assist is off.
  When one of the two is answered, the app withdraws the other.
- On the desktop the window takes no keyboard focus. Only a click or tap on the window, or closing it, counts as a response; a keypress in the middle of typing must never count as a sign of life.

### Sound and vibration

- Off by default, for foreground display and notification alike.
- The person being asked can turn on either per device. This way the phone at the bedside stays silent, for example, while the computer in the study makes a sound.
- The setting applies only locally and is not transmitted.
- The system's "Do Not Disturb" takes precedence even when sound is turned on (for
  the display on Linux see the exception above).
- On Android the system controls sound and vibration through the notification channel. The app creates the channel silent and refers to the system settings for changes.

### Notification (fallback and selectable alternative)

Dismissing counts as a response, automatic expiry never does.

| Platform | Mechanism | Note |
|---|---|---|
| Android | `deleteIntent` of the notification | Also fires on "Clear all"; this is intended. |
| Linux | D-Bus signal `NotificationClosed` | Only reason 2 (closed by the user) counts, reason 1 (expired) does not. |
| Windows | `Dismissed` event of the toast | Only `UserCanceled` counts, `TimedOut` does not. |
| iOS (later) | Category with `customDismissAction` | Without this option iOS does not report the swipe-away. |

When one device responds, the display is withdrawn on the other devices of the same person.

## Notification of an answer

The asking side learns of an answer by a notification on each of its devices: "Anna has answered" with the time, or for a reported answer with the reason ("“asleep” (reported)"). It says that an answer came, not that the person is fine.

- An ordinary notification with the sound of the system; it expires on its own, and clicking or closing it sends nothing. A click opens the app.
- Android: a channel of its own ("Answers"), default importance, with the name on the lock screen. It can be turned off in the system settings without touching the channel of requests.
- One notification per answer: the core drops an answer that arrives again. A reported answer and a later manual one are two answers; on Android the newer replaces the older notification of the same contact.

## Local interface for status reports

External programs (sleep detection, home automation, a script on the CPAP device) report a state to the app on the same device.

**Scope**
- `set(reason, duration)`: The state applies from now on for the given time span and expires on its own afterward.
- `clear()`: The state ends immediately.
- There is always at most one state; a new one replaces the old one.

**Rules**
- The report is used only after the configurable waiting time for a normal response has passed, and only if it is still valid then.
- Configurable per contact and per device whether reported responses are allowed from this device. Default: off.
- The reason is a short text that B sets personally. A sees exactly this text, so "asleep" instead of "CPAP on".
- Only the reason and the "reported" marker are transmitted, never the remaining duration or the time of the report.
- Reachable locally only: Unix socket (Linux, also through a bind mount into containers), named pipe (Windows), on Android an intent with a token behind a switch that is off by default. No network port; the only exception is the server container and Home Assistant add-on, which can open an HTTP port on request, with a token and a warning that plain HTTP exposes the token on the network.
- The report applies only on the device that received it.

**Not planned:** transmitting audio snippets or other sensor data. Android denies microphone access to services started from the background, and the feature would have high potential for misuse.

## Security and privacy

- No account, no phone number, no email address.
- Pairing via QR code or one-time code. Each contact gets its own key pair and its own mailbox; no global identifier.
- Names are never transmitted.
- Keys stay on the person's devices. End-to-end encryption with libsodium-compatible `crypto_box`.
- The relay sees only fixed-length ciphertext and random mailbox identifiers, and deletes after delivery.
- Wake signals contain no content.
- Known limit: anyone actively observing the relay sees timestamps and can infer contact pairs. Acceptable with a self-operated relay.

## Architecture

Three layers in the client:

1. **Rust core** as a standalone library: protocol, encryption, rule checking, storage. No dependency on Tauri. The relay uses the same message envelope.
2. **Tauri** for the UI, tray and the foreground window on the desktop.
3. **Kotlin layer on Android** for UnifiedPush, full-screen display and notifications. It calls the core directly, because no WebView is running when the wake signal arrives.

Further components:

- **Relay:** small Rust service in a Docker container, behind a reverse proxy with TLS and access logs turned off. Must be reachable from the internet, since the contacts are not on the own network.
- **Wake signal on Android:** UnifiedPush with a self-hosted ntfy, so that no Google service is needed. There is no ready-made plugin for Tauri; the integration is built in the Kotlin layer.
- **Desktop:** tray application with a persistent WebSocket connection to the relay.
- **Client without UI:** command-line program based on the core, for tests and for devices without a screen.

For a later publication: never hard-code the relay address, keep the wake path replaceable (UnifiedPush, FCM, APNs).

## License

- **Clients and core:** GPL-3.0-or-later with an explicit app store exception as an additional permission. Version 3, because many Rust dependencies are under Apache 2.0 and that is compatible only with version 3.
- **Relay:** AGPL-3.0-or-later, so that modified relays operated only as a service must also be disclosed.
- **Why the exception:** The GPL is a poor fit for the terms of Apple's App Store. With the exception in place from the start, later contributors automatically contribute under the same terms, and iOS remains possible.
- **Open:** the exact wording of the exception, see `license-exception.md`. It should be settled and legally reviewed before the first outside contribution.

## Source code hosting and CI

Decided: GitHub.

- **Builds:** GitHub Actions with hosted Linux and Windows runners; the Windows build thus needs no infrastructure of its own.
- **Dependencies:** Renovate instead of Dependabot, because it also runs on Forgejo.
- **Keeping a move open:** Workflows in the Actions format are largely compatible with Forgejo Actions, in case the project later moves to Codeberg.

Against Codeberg at the time of the decision: hosted Forgejo Actions only to a limited extent, Woodpecker only on request, a separate VM as runner needed for Windows.

## Order

Summary of the confirmed order; details and acceptance criteria are in `roadmap.md`.

1. Scaffolding and walking skeleton on Android. If the walking skeleton fails with Tauri, switching to Flutter is still cheap at this point.
2. Rust core and relay, operable via the command line.
3. Linux desktop, then Android app.
4. Multiple devices, Windows, local interface.
5. Hardening; later possibly iOS, macOS and publication.

## Open points

- Wording of the app store exception.
- Protection of the keys on the device (see `roadmap.md`).

## Research: similar apps

There are many daily check-in apps, but none found combines querying only on request, end-to-end encryption and desktop support.

- **Dead man's switches for people living alone** (Sileme/Demumu, Snug, Alma, leb, Corlo): fixed schedule, alarm chain when no response.
- **Social check-ins** (Knuff, Cercle, Just Checking In): continuously visible status, often with mood and chat. Knuff is the closest.
- **Technical role models** (Starling, LastSignal): end-to-end encrypted, no account, self-hostable relay.
