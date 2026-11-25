# Lebenszeichen

A notification appears. Lebenszeichen? You just tap it or wipe it like a
bug, and it's gone.

That's the whole answer. No small talk, no feeling to express, nothing
to explain and nothing to choose. It takes no technical skill and no thought
beforehand. It doesn't pull you out of what you're doing or break your train
of thought. Afterwards you remember: Someone thought of you and you've shown
them you exist. Your peer isn't troubled, you can relax.

Lebenszeichen ("sign of life") is for:

- days when you cannot manage to reply to messages,
- focused work you don't want a chat to interrupt,
- parents and children who want a quick "all fine?",
- older relatives, for whom a message thread is too much hassle.

No chat, no online status, no schedule, no alarm chain. It is not an emergency
alarm: if nobody reacts, the request only expires. But if you want, it
integrates: Home Assistant, scripts or your phone's battery can answer for you
([local interface](docs/local-interface.md)).

## Status

Milestone M1 is complete: a throwaway prototype for Android showed that a wake
signal reaches the terminated app and that the Kotlin layer can call the Rust
core; see [docs/m1-walking-skeleton.md](docs/m1-walking-skeleton.md).

Milestone M2 is complete: `crates/wire` implements the wire format from
[docs/wire-format.md](docs/wire-format.md) (plaintext of the messages,
invitation, envelope verification), `crates/core` implements encryption, pairing,
the rules for requests and the storage as a JSON file, `crates/client` the
shared client layer and `crates/cli` the command-line client `lz` (see below).
The relay (`crates/relay`,
program `lz-relay`) is configured through environment variables and can be
built as a Docker image: `docker build -f crates/relay/Dockerfile .`

Milestone M3 (Linux desktop) is done except for two checks on other
systems. M4 (Android app) is complete: phone and desktop request each other
via their own relay with wake signals, and the CI builds a signed APK. M5
(multiple devices) is complete: a person's phone and computer share contacts,
an answer on one withdraws the request on the other, and new devices are added
with a QR code and two check codes. M6 (Windows) is done: an installer from
the CI, notification and foreground window, start at login; still to check
is Do Not Disturb on Windows 11. M7 to M8b (answer notification, local
interface, container and Home Assistant) are complete; M8c (handing a state
to the own devices) is done except its final test. M9 (hardening) is done except for a review and a guide test by
a person: [docs/self-hosting.md](docs/self-hosting.md) describes running your
own relay. Status in [docs/roadmap.md](docs/roadmap.md).

| Milestone | Content |
|---|---|
| M0 | Scaffolding, CI, Renovate, license files |
| M1 | Walking skeleton on Android |
| M2 | Core and relay, operable via the command line |
| M3 | Linux desktop |
| M4 | Android app |
| M5 | Multiple devices |
| M6 | Windows |
| M7 | Notification when an answer arrives |
| M8 | Local interface for status reports |
| M9 | Hardening |

## Command line

`lz` is the client without a UI. Each device has a state file, selectable
with `--state <path>` or `LZ_STATE`; the default is
`$XDG_DATA_HOME/lebenszeichen/device.json` (otherwise
`~/.local/share/lebenszeichen/device.json`), on Windows
`%APPDATA%\lebenszeichen\device.json`.

```sh
# Once: create the device. With an access token it can invite.
lz init --relay https://relay.example.org --access-token <b64url>

# Anna invites Bert and gives him the code (valid for 10 minutes).
lz invite Bert              # with --voucher Bert may create a mailbox on Anna's relay
# Bert accepts; the six-digit verification code follows once Anna's device answers.
lz accept Anna LZ2-…
# Anna picks up the acceptance; her device answers right away, Bert picks up the code.
lz sync                     # on both devices
# Both compare the code in person or on the phone, then Anna confirms.
lz confirm Bert             # if the verification code does not match: lz reject Bert
# Bert picks up the confirmation and Anna's rules.
lz sync

lz request Anna             # ask for a sign of life
lz respond Bert             # respond to the open request
lz report Bert "asleep"     # report a state; the request stays open
lz rules Bert --interval-min 360 --expiry-min 1440 --weekly 7
lz contacts                 # contacts, open requests, "request possible again from …"
lz listen                   # stay connected and show messages immediately
lz remove Bert              # remove contact; Bert sees this on the next poll
lz push https://ntfy.example.org/upXYZ  # register the push address the relay uses to wake the device
lz push --remove            # remove the push address
lz access-token <b64url>    # take a new access token after the relay's was replaced
```

`lz sync` picks up everything waiting and exits, `lz listen` stays connected.
`--verbose` also shows discarded messages. A device without an access token
can only accept invitations with a voucher. `http://` addresses are allowed
for local tests; in operation the relay sits behind TLS. The operating system
verifies the certificates.

The invitation must be accepted within 10 minutes. The inviting side can pick
up the acceptance later, up to 24 hours afterward.

## Principles

- The app is not a surveillance tool. The person being asked determines how
  often that may happen, and their own device enforces it.
- The app is not an emergency call system. No response does not mean "emergency".
- No account, no phone number, no email address.
- End-to-end encrypted. The relay sees only fixed-length ciphertext.

## Structure

```
crates/
├── wire/    Message envelope and relay interface (no_std)
├── core/    Protocol, encryption, rule checking, storage
├── client/  Relay access, commands, polling; used by lz and the app
├── cli/     Client without a UI (program: lz)
└── relay/   Relay server (program: lz-relay)
app/
├── src-tauri/   Tauri shell for the Linux and Windows desktop and Android
├── ui/          UI for both (plain HTML without a build step)
└── android/     Kotlin layer: UnifiedPush, full-screen display, notifications
docs/        Concept, protocol, roadmap, walking skeleton M1, Android release, Windows release, draft of the license exception
```

`app/` is not part of the Cargo workspace. The desktop app is built and
started locally, on Linux with WebKitGTK 4.1 and GTK 3, on Windows with the
Visual Studio Build Tools (WebView2 is part of Windows 10 and 11):

```sh
cd app/src-tauri
cargo build
LZ_STATE=/path/to/device.json target/debug/lebenszeichen-app   # LZ_STATE optional
```

The relay address prefilled in the setup screen comes from the build-time
variable `LZ_DEFAULT_RELAY`, so that the address is not in the code
(`LZ_DEFAULT_RELAY=https://relay.example.org cargo build`). The CI reads it
from the GitHub repository variable of the same name
(`gh variable set LZ_DEFAULT_RELAY -b https://relay.example.org`); unset or
empty means no prefill.

Without `LZ_STATE` the state lives in the app's data directory
(`~/.local/share/io.github.irgendwer92.lebenszeichen/device.json`); the
desktop app is thus a separate device alongside `lz`.

The Android APK is built by the CI workflow "Android debug" on pull requests
that touch `app/` or `crates/`, or by hand:
`gh workflow run android-prototype.yml --ref <branch>`. The finished APK is
attached there as an artifact. The signed APK is built by the workflow "Android release" on
a tag `v*`; setup and verification in
[docs/android-release.md](docs/android-release.md).
The Windows installer (unsigned, per user) is built by the workflow "Windows release";
see [docs/windows-release.md](docs/windows-release.md).

## Development

A current stable Rust toolchain is required.

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check        # needs: cargo install cargo-deny
```

The same steps run on every push and pull request in the CI, for
`app/src-tauri` in a separate step (desktop app for Linux).
Identifiers in the code are English, the documentation is English.

Texts for people live in Fluent files, `locales/<tag>/lebenszeichen.ftl`
(English is the default and the fallback, German is complete), and are
embedded by `crates/client/src/i18n.rs`. `lz` picks the language from
`LC_ALL`, `LC_MESSAGES` or `LANG`. To add a language, copy
`locales/en/lebenszeichen.ftl` to `locales/<tag>/`, translate the values, add
a variant to `Language` and to `Language::ALL` in `i18n.rs`; the tests check
that the file parses and has every message id of the English one. The app
follows the system language by default; ⚙ → Language sets it per device
(`settings.json` next to the state file, never synced). On the desktop the
app turns "Start at login" on at the first start, because it only shows
requests while it runs; it then starts straight into the tray (`--hidden`).
⚙ → Start at login turns it off, and it stays off.

## License

- `crates/wire`, `crates/core`, `crates/cli` and the app: GPL-3.0-or-later,
  see [LICENSE](LICENSE). An app store exception is planned, the wording
  is available as a draft in [docs/license-exception.md](docs/license-exception.md).
- `crates/relay`: AGPL-3.0-or-later, see
  [crates/relay/LICENSE](crates/relay/LICENSE).
