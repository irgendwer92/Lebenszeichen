# Review brief

For a person reviewing the encryption and the relay of Lebenszeichen. It
says what the system is meant to protect, where to look and what is known
already. Status: M9, 2026-11-19.

## What it is

A person asks a contact for a sign of life; the contact answers with one tap
(or a device reports a state such as "asleep" for them). Devices talk only
through a relay that passes encrypted messages of a fixed size; phones are
woken through UnifiedPush (ntfy). A person may have several devices that
share their contacts. Everything is self-hostable; there are no accounts.

Read first: `concept.md` (purpose, section "Security and privacy"),
`protocol.md` (message flow, what the relay stores and sees),
`wire-format.md` (every byte, keys, pairing, device handover, relay
interface, earlier decisions).

## What should hold

1. **Content.** Only the two paired devices (and the other devices of the
   same person) can read or produce requests, answers, rules, reported
   states and names. Names are never transmitted at all.
2. **Pairing.** Someone who controls the channel an invitation is passed on
   (a messenger), or the relay operator, cannot pair in place of the
   intended contact without the six-digit check code differing.
3. **Adding a device.** A device can only join a person's group with the
   person confirming two check codes; a planted handover is detected.
4. **Replay and order.** Old or repeated messages are not applied twice;
   a request cannot be replayed to trigger an alarm again.
5. **Relay.** Without the access token or a voucher nobody can create
   mailboxes; without a mailbox's tokens nobody can read from or write to
   it. The relay cannot be used to reach internal addresses (push URLs).
   It stores only what `protocol.md` lists and keeps no request logs.
6. **Local interface.** Only the local user (socket and pipe permissions) or
   an app holding the token (Android intent, optional HTTP port of the
   container) can report a state.

## Where to look

| Area | Code |
|---|---|
| Wire format, parsing of untrusted bytes, invitation and setup codes | `crates/wire/src/` |
| Encryption (`crypto_box`, `crypto_box_seal`, `crypto_secretbox`, RustCrypto) | `crates/core/src/crypto.rs` |
| Pairing, check code, time window | `crates/core/src/pairing.rs` |
| Contact state, replay protection, rules | `crates/core/src/contact.rs` |
| Sync group and adding a device | `crates/core/src/group.rs`, `handover.rs` |
| Storage of keys (JSON file, permissions) | `crates/core/src/store.rs` |
| Relay: API, tokens, limits, push URL check, stream | `crates/relay/src/` |
| Client: delivery, resending (`Device::unsent`), access token | `crates/client/src/ctx.rs`, `relay/http.rs` |
| Local interface (socket, token) | `crates/client/src/local.rs`, `crates/server/` |

Tests: `cargo +stable test --workspace`; the client tests run against a fake
relay (`crates/client/src/relay/fake.rs`), the relay tests against a real
one (`crates/relay/tests/relay.rs`).

## Known limits (accepted)

- **The relay sees metadata**: when devices connect, which mailboxes are
  active at the same time, and so can guess who is paired with whom. Meant
  for self-hosted relays.
- **A lost device keeps the contact keys.** Removing it stops wake signals
  and ends the group of own devices (so it cannot follow new pairings), but
  a copy of its state can read and answer on the old contact mailboxes
  until the contacts are paired again (`wire-format.md`, "Removing a
  device"). Re-keying contacts on removal is later.
- **Keys at rest** are protected only by file permissions and the operating
  system (Android app storage, no backup); no encryption of the state file
  yet (open point in `roadmap.md`).
- **Clocks.** Messages carry the sender's time and are accepted from 24 hours
  10 minutes in the past to 10 minutes in the future; devices warn when
  their clock differs from the relay's by more than 5 minutes.
- **Wake signals** go to whatever public https address a device registers;
  `LZ_PUSH_ALLOWED_HOSTS` can restrict them.

## Earlier reviews

- 2026-11-16, crypto review of M5 (by a separate AI session): the version-1
  check code could be matched by searching about a million keys; fixed by
  the commitment in invitation version 2 (`wire-format.md`, "Decisions for
  the pairing fix"). The device check code got a commitment and a challenge
  after a second review.
- 2026-11-19, review of M9 (separate AI session): results in
  `m9-hardening.md`.
- A review by a person is still open.

## How to report

An issue on GitHub for anything that is not a vulnerability; a
vulnerability privately, as `SECURITY.md` describes.
