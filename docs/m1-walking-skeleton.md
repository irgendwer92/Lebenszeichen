# Lebenszeichen – M1: Walking skeleton on Android

As of 2026-11-14. Belongs to `concept.md` and `roadmap.md`.

M1 is a throwaway prototype. It answers a single question: Does the decision for Tauri on Android hold up, even though no WebView is running when the wake signal arrives? If not, the roadmap says to switch to Flutter here.

**Result: The walking skeleton worked on a device.** All ten runs of the test protocol passed on a Fairphone 4. The two overnight runs were first done on 2026-11-13 with one hour of rest, and then, in the night from 2026-11-13 to 2026-11-14, according to the protocol. Details are in the section [Test result of 2026-11-13](#test-result-of-2026-11-13); what is still open is in the section [Not verified](#not-verified).

## What the prototype does

1. The app registers with a distributor through UnifiedPush (the ntfy app is the intended one) and shows the endpoint.
2. An HTTP POST to the endpoint is the wake signal. With it, the distributor starts the app, even if the app has been quit.
3. The Kotlin layer calls the function `prototype::marker` in the Rust core through JNI. Tauri, activity and WebView do not run during this.
4. The request appears as a silent notification with a full-screen intent: full screen if the device is locked or the screen is off, otherwise as a banner. No sound, no vibration.
5. Each of these reactions sends exactly one HTTP POST to a configurable address:
   - Tap the notification (`tap`)
   - Swipe the notification away, also via "Clear all" (`dismiss`)
   - Tap the full-screen display (`fullscreen-tap`)
   - Close the full-screen display with "Back" (`fullscreen-close`)
   - Leave the full-screen display via the Home button or the overview of recent apps (`fullscreen-leave`, added after the first device test)
6. The text of the POST comes from the Rust core, for example:

   ```
   lebenszeichen-m1 core=rust wire=1 event=dismiss request=r20261113-203300-a1b2
   ```

   A received POST with `core=rust` therefore proves that the core ran in the woken process. If the Kotlin layer does not reach the core, the text contains `rust-core-unavailable` with the reason instead.
7. A log in a file records every step with a timestamp and survives the end of the process.

Does not count as a response: the screen turns off on its own, a banner disappears on its own. The notification then stays and can be answered later. The prototype never lets notifications expire.

## What the prototype does not do

- No encryption, no contacts, no relay, no rules for requests. That is M2 and M4.
- The content of the wake signal is not read. Every wake signal creates a new request with its own ID.
- The response goes unencrypted to the configured address. It contains only the marker, the kind of reaction and the ID of the request.
- No expiry after 24 hours, no withdrawal on other devices.
- No custom app icon (Tauri's default icon), no translations, no accessibility check.
- Android only, arm64 only, debug build only.

## Structure

```
app/
├── src-tauri/   Tauri shell (Rust). Contains the JNI function for the Kotlin layer.
├── ui/          User interface: plain HTML, CSS and JavaScript without a build step
├── android/     Kotlin layer as a local Tauri plugin
│   ├── src/         Rust side of the plugin (only registers the Kotlin class)
│   └── kotlin/      Android library module: manifest, Kotlin code
└── package.json Only for the Tauri CLI
crates/core/     Placeholder function prototype::marker
```

The path of a request:

```
curl ──POST──▶ ntfy server ──▶ ntfy app (distributor)
                                   │ broadcast
                                   ▼
        Receiver of the connector library ──▶ PushServiceImpl.onMessage
                                                   │
                           Requests.open ──JNI──▶ lz_core::prototype::marker
                                   │
                    RequestPresenter.show: notification with full-screen intent
                                   │
      ┌────────────────────────────┼─────────────────────────────┐
      ▼                            ▼                             ▼
   tap                          swipe away                  full-screen display
 (ReactionReceiver)           (ReactionReceiver)           (RequestActivity)
      └────────────────────────────┼─────────────────────────────┘
                                   ▼
        Requests.react: once per request ──JNI──▶ marker ──▶ AnswerWorker
                                                                  │ HTTP POST
                                                                  ▼
                                                            response address
```

The user interface is not needed for this path. It only shows state and log and talks to the Kotlin class `LebenszeichenPlugin` through Tauri commands.

## Decisions

**Kotlin layer as a local Tauri plugin.** A Tauri plugin may bring an Android library module along. Tauri includes it in the generated Gradle project at build time; the module's manifest (service, receiver, activity, permissions) and dependencies are merged in the process. Nothing in the generated project therefore has to be changed by hand. Deviation from the roadmap: The Kotlin code lives under `app/android/kotlin/` instead of directly under `app/android/`, because the plugin's Rust package lives in the same folder.

**The Android project is not checked in.** CI regenerates `app/src-tauri/gen/android` on every run with `tauri android init`. For a throwaway prototype this means: no generated Gradle files in the repository, and the project always matches the Tauri CLI in use. The price: Changes to the generated project are not possible; the app icon and `targetSdk` come from Tauri's template. For M4 the project should be checked in, as Tauri intends.

**One shared native library.** The JNI function lives in the Tauri app's package (`app/src-tauri/src/jni_bridge.rs`) and thus in the same library `liblebenszeichen_lib.so` that Tauri puts into the APK anyway. The Kotlin layer loads it itself with `System.loadLibrary`. Reasons:
- The library ends up in the APK without an additional build step.
- This is exactly the risk M1 is meant to clarify: Can the Tauri library be loaded and used without Tauri running? According to the source code, yes: Neither Tauri nor `wry` or `tao` has a `JNI_OnLoad`; on loading, only a harmless constructor from `tauri-utils` runs, which remembers the path of the program.
- The function is not in `lz-core`, because JNI exports need `unsafe` attributes and the workspace forbids `unsafe_code`. `lz-core` stays free of Android.

After building, CI checks that the symbol `Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_marker` is exported in the library in the APK. The name of the library appears in two places and must stay the same: `[lib] name` in `app/src-tauri/Cargo.toml` and `LIBRARY` in `CoreBridge.kt`.

**App outside the workspace.** `app/` is excluded in the root `Cargo.toml`. On Linux, Tauri builds only with WebKitGTK and brings a large dependency tree. This keeps the "Check" workflow (then named "Prüfen") just as fast. Consequence: `cargo deny`, Clippy and tests do not run over the code under `app/`.

**UnifiedPush through the official connector library.** `org.unifiedpush.android:connector` in the service variant (`PushService`), as the current documentation provides; the receiver comes from the library. No embedded FCM distributor, no Google services. The prototype picks the distributor without asking: the saved one, otherwise ntfy (`io.heckel.ntfy`), otherwise the first one found.

**Nothing to do after a restart.** The registration lives with the distributor and in the app's data. The distributor starts by itself after the restart and wakes the app by broadcast. The library does not require its own receiver for system boot. The documentation only recommends renewing the registration regularly; this happens every time the user interface is opened.

**Response through WorkManager.** The receiver for tap and swipe lives only a few seconds. It therefore does not send by itself but hands the response to WorkManager and waits only until the job is stored. WorkManager waits for a network connection, retries after network errors (at most six attempts, growing intervals) and survives the end of the process and a restart. From Android 12 on, the job runs as an expedited job and starts immediately. There is deliberately only this one path: An additional direct attempt could lead to two POSTs on a timeout.

**Exactly one response per request.** The first reaction synchronously sets a flag in the app's settings; every further reaction to the same request is only logged (`reaction-ignored`). In addition, WorkManager accepts only one job per request. A remainder is left: If the server accepts the POST but the acknowledgment is lost, the retry sends it a second time. The ID of the request is therefore in the text.

**Silent, but important.** The notification channel has high importance, because otherwise Android shows neither full screen nor banner. Sound and vibration are switched off on the channel. The category is "reminder", not "alarm": Alarms may break through Do Not Disturb in the default setting, and the concept requires that Do Not Disturb be respected.

**Home button counts.** In the first version, the prototype counted only tap and "Back" in the full-screen display. After the device test, it was added that leaving via the Home button or the overview also counts (`onUserLeaveHint`). Android calls this method only on a deliberate leave, not when the screen turns off by itself. This addition is built but not yet verified on a device.

## Versions

| Component | Version |
|---|---|
| Tauri | 2.12.5 (`tauri-build` 2.7.4, `tauri-plugin` 2.7.2) |
| Tauri CLI | 2.12.4 (npm package `@tauri-apps/cli`) |
| `jni` | 0.21.1 |
| UnifiedPush connector | 3.3.5 |
| WorkManager | 2.10.0 |
| Android Gradle plugin, Kotlin, Gradle | 9.3.1, 2.2.10, 9.6.1 (from the template of the Tauri CLI) |
| `compileSdk` / `targetSdk` / `minSdk` of the app | 37 / 37 / 24 (from the template of the Tauri CLI) |
| NDK, JDK, Rust | 30.0.16248370, Temurin 21, 1.100.0 (from the CI runner, not pinned) |

## Getting and installing the APK

1. On GitHub: **Actions → Android-Prototyp →** latest green run **→ Artifacts →** download `lebenszeichen-m1-arm64-debug-apk` and unpack it. It contains `lebenszeichen-m1-arm64-debug.apk`. With the GitHub CLI:

   ```sh
   gh run download --repo irgendwer92/Lebenszeichen --name lebenszeichen-m1-arm64-debug-apk
   ```

2. Install, either via USB:

   ```sh
   adb install -r lebenszeichen-m1-arm64-debug.apk
   ```

   or copy the file to the phone, open it there and allow the file manager to install from unknown sources.

The workflow starts on every pull request that changes `app/`, `crates/` or the workflow itself, and manually via "Run workflow". Artifacts are kept for 30 days.

**About the signature:** Debug builds are signed with a debug key that the Android Gradle plugin generates on the CI runner. The workflow keeps this key in the GitHub Actions cache so that consecutive builds have the same signature and can be installed over each other. This is only a stopgap: The cache applies per branch or pull request and expires after seven days without access. If Android reports on installing that the package conflicts with an existing app, the signature has changed. Then uninstall the old app and install again. Registration, settings and log are lost in the process, and the endpoint changes. The checksum of the certificate is in the CI run, in the step "Check the APK" under "Signature and versions". The debug key is not a secret and protects nothing; the proper signature comes with M4.

## Setup

You need an ntfy server (your own or a public one) and the ntfy app as distributor.

1. **Install the ntfy app**, preferably from F-Droid; this variant works without Google services. In ntfy, enter your own server as the default server if you use one, and exempt ntfy from battery optimization. Open the app once.
2. **Open Lebenszeichen** and set everything under "Berechtigungen" to "Ja":
   - Allow notifications (from Android 13 the system asks).
   - Full-screen display: a separate permission from Android 14. With sideloading it is usually already granted; otherwise "Einstellungen" leads to the right page.
   - Battery optimization: "Ausnehmen".
3. **Tap "Registrieren".** After a few seconds, an address of the form `https://<ntfy-server>/up…?up=1` appears under "Endpunkt". If the field stays empty, the reason is in the log.
4. **Enter the response address and save it**, for example an ntfy topic with a hard-to-guess name: `https://<ntfy-server>/lz-test-<random>`. The topic must be writable without login; the prototype sends no credentials. Do not subscribe to this topic in the ntfy app on the same phone, otherwise every response creates another notification there. The response address must differ from the endpoint from step 3: If both are the same, every response wakes the device again, and nothing arrives on the computer. The prototype does not check this.
5. **"Anfrage lokal auslösen"** (trigger a request locally; a prototype button, removed in M4) as a first test: A banner appears; tap it. In the log, `local-trigger`, `core`, `shown`, `reaction`, `core`, `answer-queued`, `answer-sent` follow.

### Sending the wake signal and watching responses

On the computer, follow the responses in one window (one line per response):

```sh
curl -sN "https://<ntfy-server>/lz-test-<random>/raw"
```

In a second window, wake the device. The app copies the ready-made command via "curl-Befehl kopieren":

```sh
curl -d "wecken" "https://<ntfy-server>/up…?up=1"
```

The content is arbitrary but should not be empty. For time measurements:

```sh
date +%T; curl -d "wecken" "https://<ntfy-server>/up…?up=1"
```

## Log in the app

Each line: time, ID of the request, event, details. Newest first. "Kopieren" puts the whole log in the clipboard. The same lines appear in `adb logcat -s Lebenszeichen`.

| Event | Meaning |
|---|---|
| `register`, `endpoint`, `registration-failed`, `unregistered`, `unregister` | Registration with the distributor |
| `wake` | Wake signal arrived through UnifiedPush |
| `local-trigger` | Request through the button in the app |
| `core` | Return of the Rust core; `core=rust` means the call worked |
| `shown` | Notification posted, with state: screen on, locked, full screen allowed |
| `not-shown` | Display not possible, with reason |
| `fullscreen-shown` | Full-screen display was actually opened |
| `reaction` | First reaction, with kind |
| `reaction-ignored` | Further reaction to an already answered request |
| `answer-queued` | Response handed to WorkManager |
| `answer-sent` | HTTP POST accepted, with status code and attempt |
| `answer-retry`, `answer-failed` | POST failed, with reason |

## Test protocol

Acceptance criterion from the roadmap: It works on a real device ten times in a row, also after a restart and overnight.

Preparation: set up as above, watching the responses is running, log in the app cleared. "App quit" means: swiped away from the list of recent apps. Do not use "Force stop" in the settings; it blocks the app from wake signals until it is opened again (see Pitfalls).

A run counts as passed if all four points hold:

- **A** The display appears in the expected form.
- **B** Exactly one POST arrives on the reaction, with `core=rust` and the expected reaction.
- **C** The log shows the chain `wake` → `core` → `shown` → `reaction` → `core` → `answer-queued` → `answer-sent` for the request.
- **D** Without a reaction, no POST arrives.

| No. | Starting situation | Reaction | Expected display | Expected POST | Wake signal sent | Display appeared | POST arrived | Passed |
|---|---|---|---|---|---|---|---|---|
| 1 | App quit, screen off and locked | Tap full screen | Full screen | `fullscreen-tap` | | | | |
| 2 | App quit, screen off and locked | Close full screen with "Back" | Full screen | `fullscreen-close` | | | | |
| 3 | App quit, screen off and locked | Do nothing for 2 minutes, then unlock and swipe the notification away | Full screen, then notification | None in the 2 minutes, then `dismiss` | | | | |
| 4 | App quit, screen on, another app open | Tap banner | Banner | `tap` | | | | |
| 5 | App quit, screen on, another app open | Let the banner disappear, open the shade, swipe away individually | Banner | First none, then `dismiss` | | | | |
| 6 | App quit, screen on, another app open | Open the shade, "Clear all" | Banner | `dismiss` | | | | |
| 7 | Two wake signals in quick succession, app quit, screen on | Tap one, swipe one away | Two notifications | Two POSTs with different IDs: `tap`, `dismiss` | | | | |
| 8 | Device restarted and unlocked once, Lebenszeichen not opened, waited 5 minutes | Any | Depending on screen | Matching the reaction | | | | |
| 9 | Overnight: device lies still for at least 6 hours, not on the charger, screen off. Send the wake signal in the morning, before the device is touched | Tap full screen | Full screen | `fullscreen-tap` | | | | |
| 10 | Overnight: send the wake signal in the evening, do not touch the display. Unlock in the morning | Swipe the notification away | Full screen in the evening, notification in the morning | None in the evening, `dismiss` in the morning | | | | |

For each run, note:

- Time of the wake signal (`date +%T` before the `curl`)
- Time of `wake` and `shown` in the log, and from that the delay
- Time at which the POST arrived on the computer, and its text
- Number of POSTs for this ID
- for 9 and 10 additionally: battery level, Wi-Fi or mobile data

Note once: device, Android version and manufacturer's user interface (shown in the app under "This device"), version of the ntfy app, ntfy server, number of the CI run of the APK.

**Evaluation:**

- All ten passed: M1 is met, the decision for Tauri stands.
- If a run fails, first find in the log the point where the chain breaks:
  - No `wake`: The wake signal did not reach the app. This is due to the distributor, battery saving or delivery, not Tauri. With Flutter it would be the same.
  - `wake`, but `rust-core-unavailable` instead of `core=rust`: The core cannot be called in the woken process. This is the question M1 is about.
  - `shown`, but nothing visible: Check permissions and Do Not Disturb.
  - `reaction`, but no `answer-sent`: Network or WorkManager; `answer-retry` and `answer-failed` give the reason.
- For the abort criterion, only failures count that are due to the interplay of Tauri, Kotlin layer and Rust core. What Android imposes on every app affects Flutter just as much.

## Test result of 2026-11-13

Tested by the project owner with the APK from CI.

| | |
|---|---|
| Device | Fairphone 4 |
| System | /e/OS 4.4.2 based on Android 16 (official build), Kvaesitso as home screen |
| Additionally checked | with battery saver and data saver switched on |
| Distributor | ntfy app with the public server ntfy.sh |

| No. | Starting situation (short form) | Passed |
|---|---|---|
| 1 | Locked, tap full screen | yes |
| 2 | Locked, close full screen with "Back" | yes |
| 3 | Locked, wait 2 minutes, then swipe away | yes |
| 4 | Screen on, tap banner | yes |
| 5 | Screen on, let banner disappear, then swipe away | yes |
| 6 | Screen on, "Clear all" | yes |
| 7 | Two wake signals, tap one, swipe one away | yes |
| 8 | After restart, without opening the app | yes |
| 9 | Rest period, wake signal afterwards | yes; repeated in the night from 2026-11-13 to 2026-11-14 according to the protocol (at least 6 hours, not on the charger, screen off) |
| 10 | Wake signal before the rest period, reaction afterwards | yes; repeated in the night from 2026-11-13 to 2026-11-14 according to the protocol (at least 6 hours, not on the charger, screen off) |

The times were consistently in the expected range, almost without delay. Individual times were therefore not noted per run. An example pair (topic names shortened):

```
$ date +%T; curl -d "wecken" "https://ntfy.sh/up…?up=1"
22:29:24

$ curl -sN https://ntfy.sh/lz-test-…/raw
lebenszeichen-m1 core=rust wire=1 event=fullscreen-close request=r20261113-222924-d89e
```

The ID of the request carries the time at which the device was woken: the same second as the send. `core=rust` proves that the Rust core was called in the woken process without a running WebView.

**Assessment:**

- The question of M1 is answered for this device: Waking, calling the core, full screen and banner, response on tap, close and swipe away work with Tauri and the Kotlin layer. There is no reason to switch to Flutter.
- The "overnight" acceptance criterion is met. On 2026-11-13, rows 9 and 10 first ran with only one hour of rest; in the night from 2026-11-13 to 2026-11-14, both were repeated according to the protocol and passed.
- Do Not Disturb (checked afterwards): The request then appears only in the notification shade, without full screen and without banner. This matches the concept.
- Network outage (observed, not tested deliberately): After the network is switched off, the request arrives shortly after the network is back. The retry of the response after a network error remains unverified.
- Other devices: A field test with two Android devices is planned for later.
- M1 is complete.
- During setup, endpoint and response address were initially the same. The note on this is now under "Setup".

## Pitfalls on Android

- **Manufacturers' battery saving.** Xiaomi, Huawei, Samsung, OnePlus, Oppo and others terminate apps in the background more aggressively than Android itself and in part prevent them from being woken. Turn off battery optimization for Lebenszeichen and for ntfy; depending on the manufacturer, additionally allow "autostart" and protect the app from cleanup. Instructions per manufacturer are collected by dontkillmyapp.com.
- **"Force stop".** After it, Android delivers no more broadcasts to the app until it is opened by hand. Some manufacturers treat even swiping it away from recent apps this way. If this shows up in the test in runs 1 to 7, it is a quirk of the device.
- **Doze.** When the device lies still, Android bundles network access into maintenance windows. Whether a wake signal arrives immediately depends on whether the distributor may keep its connection; this is why ntfy must be exempt from battery optimization. Delays of minutes are possible in deep sleep and are not a fault of the prototype. The response itself runs as an expedited job from Android 12.
- **Full screen only when the device is locked.** If the screen is on and unlocked, Android shows a banner instead of the full screen. This is intended and cannot be changed.
- **Permission for full screen.** From Android 14 it can be revoked; then only a notification appears even when the device is locked. The app shows the state.
- **Pushing the banner up** is not swiping away: The notification only moves to the shade and stays open. Only swiping it away there or to the side counts.
- **Do Not Disturb.** The request is then placed silently in the shade, without full screen and without banner. This is intended. Whoever wants it otherwise allows reminders in the Do Not Disturb settings or exempts the app.
- **Lock screen.** If notifications are hidden on the lock screen, only the full-screen display remains.
- **Channel settings are permanent.** Android fixes the sound, vibration and importance of a channel when it is first created; after that only the user can change them. Whoever has fiddled with the channel gets the defaults back only by reinstalling. The app shows whether the channel still has high importance.
- **Unlock once after a restart.** Before the first unlock, the apps' data is encrypted; neither ntfy nor Lebenszeichen run then.
- **`targetSdk` 37.** Tauri's template targets the newest Android version. Behavior that Android ties to the target SDK may therefore differ from older guides.

## Not verified

Verified: The Rust part of the workspace passes formatting, Clippy and tests. The JNI function was called from a JVM on a Linux machine and returns the expected text. CI builds the APK and checks that the JNI symbol is exported, that service, receiver, activity and permissions are in the merged manifest, and that the classes of the Kotlin layer are in the APK. Two consecutive CI runs in the same pull request produced APKs with the same signature.

The following list dates from before the first device test. The test of 2026-11-13 showed on one device that the app starts, the user interface works, registration and reception through UnifiedPush with ntfy work, the core can be called in the woken process, display and reactions are correct, exactly one POST is created, and nothing needs to be done after a restart.

Still open are: other devices and manufacturers, the reaction through the Home button, the retry after network errors, Android below 12 and the licenses of the Android libraries (connector, WorkManager, Tink). The app's Rust dependencies have been checked by CI since M3.

The original list, for context:

- **The app as a whole.** Whether it starts and the user interface loads.
- **The user interface's commands.** Names of the commands, authorization through Tauri's permissions, passing of arguments. An error shows up as a message at the top of the app.
- **Registration and reception through UnifiedPush**, also with ntfy as distributor. In particular: that ntfy passes an unencrypted text through and the library reports it as a message, and that the library reaches the non-exported service on current Android versions. The library's source code was read in version 3.0.10; the build uses 3.3.5, whose source code was not available here. The interface is the same according to the API documentation.
- **The core of the walking skeleton:** that `System.loadLibrary` and the JNI call work in the woken process without an activity. All that is established is that nothing in the source code speaks against it.
- **The display:** full screen when the device is locked, banner without sound and vibration at high importance, behavior under Do Not Disturb, appearance of the full-screen display (margins, dark mode).
- **The reactions:** that swiping away and "Clear all" trigger the `deleteIntent`, but a banner disappearing does not; that "Back" arrives in the full-screen display; that exactly one POST is created.
- **WorkManager:** start from the woken process, expedited jobs from the background, retry after network errors.
- **Restart, Doze, manufacturers' battery saving.** The statement "nothing to do after a restart" rests on the library's documentation and source code.
- **Android below 12.** There the response runs as an ordinary job and may be delayed. Android below 8 (without channels) was not considered at all.
- **Licenses of the app dependencies.** `cargo deny` does not run over `app/`. The dependencies of Tauri and the Android libraries (connector, WorkManager, Tink: Apache-2.0) have not been formally checked against GPL-3.0-or-later.

Also not tried, and therefore without guarantee, are the notes on the ntfy app (F-Droid variant, default server).

## What happens next

- The test is passed, also overnight; the decision for Tauri stays.
- The prototype stays untouched until M4 replaces it. The findings are carried over, not the code.
- Open for M4: check in the Android project, own signature, app icon, expiry after 24 hours, sound and vibration through the system settings of the channel.
