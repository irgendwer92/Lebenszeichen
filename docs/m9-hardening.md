# M9 – Hardening

Done when: someone without prior knowledge can set up a relay and pair two
devices using the guide (roadmap).

## Decisions (2026-11-17)

1. **Relay image on GHCR.** `container.yml` gets a second job that builds
   `crates/relay/Dockerfile` and pushes `ghcr.io/irgendwer92/lebenszeichen-relay:<version>`
   (amd64 and arm64, as the VPS is arm64). Self-hosters pull it instead of
   building; building from source stays documented.
2. **Guide for self-hosters** in `docs/self-hosting.md`, with ready files in
   `deploy/relay/`: `compose.yaml` (relay and ntfy), `relay.env.example`,
   `ntfy/server.yml`, and one reverse proxy example each for **nginx, Caddy and
   Traefik**, all with access logs off for both hosts.
3. **Clock offset warning, built.** The client compares the `Date` header of
   every relay answer (HTTP and the WebSocket handshake) with its own clock.
   Above 5 minutes (half of the 10 minutes a message may be ahead) it warns:
   the app shows a banner, `lz` prints a line, the Android log notes it. The
   warning goes away with the next answer within the limit. Nothing is sent or
   stored for it; the relay already sends `Date`.
4. **Review.** An independent review of the encryption (`wire`, `core`) and the
   relay by a separate Opus session stands in for the second person for now,
   on the final code of M9. Findings are fixed or recorded here. A review
   brief (`docs/review-brief.md`: threat model, scope, known limits) is written
   for a human reviewer; the human review stays open after M9.
5. **Done criterion.** First a dry run: a fresh session that gets only the
   guide sets up relay and ntfy in local Docker and pairs two `lz` devices;
   every point where it gets stuck is fixed in the guide. The real check is a
   person following the guide on their own server.

## Guide outline (`docs/self-hosting.md`)

1. What you need: a server reachable from the internet, a domain with two
   names (relay, ntfy), Docker with compose, ports 80 and 443.
2. Why both must be public: wake signals go only to public addresses; the NAT
   loopback note from the roadmap.
3. Access token: create it (`openssl rand` → b64url), keep it secret; who
   holds it can create mailboxes.
4. Start relay and ntfy from `deploy/relay/`.
5. Reverse proxy: nginx, Caddy or Traefik, each with logs off; WebSocket
   upgrade for the relay.
6. ntfy: deny-all, everyone write-only on `up*`, one read account for the
   phones; set it in the ntfy Android app.
7. First device: setup with relay address and access token, or a setup code
   as QR for further devices of the token holder.
8. Second person: invitation with voucher (no token needed), compare the check
   code in person or on the phone.
9. Check: request and answer both ways, wake signal with the app closed.
10. Operation: backup of the database, update, what the relay stores and what
    it cannot see.
11. Lost or stolen device: remove it on the other own devices, then pair the
    contacts again (removing alone does not lock it out).
12. Troubleshooting: certificate errors, no wake signals, clock warning,
    `unknown_mailbox`.

## Behaviour tests

| Case | What is checked | How |
|---|---|---|
| Relay outage | a request, an answer, a sync message and a pairing step sent while the relay is down go out once it is back, not twice; the app shows "relay unreachable" | tests with the fake relay, then by hand with `docker stop` |
| Relay restart | messages waiting in mailboxes survive; live connections reconnect | relay tests, by hand |
| Clock deviation | a device 15 minutes ahead or behind: its messages are refused as today, and the new warning appears on it | tests with a fake `Date`, by hand with a shifted clock in a container |
| Lost device | after removal and pairing again, the removed device can neither read nor send to the new mailboxes | client tests |

Tests: `crates/client/src/tests/behaviour.rs` and two in
`crates/relay/tests/relay.rs` (2026-11-19). The fake relay can lose the
answer to a delivery after storing it, and the relay tests can stop and
restart a real relay on the same database.

Behaves as expected: a request, response, confirm or sync message sent while
the relay is down fails visibly, changes nothing (the weekly limit is not
used up) and goes out once later; invite and accept leave nothing behind; a
lost `reveal` is sent again for a repeated `hello`; waiting messages, readers
and sequence numbers survive a relay restart, and a live connection gets
what arrived meanwhile.

Gaps found and fixed:

1. **Lost answer to a request.** The relay stored it, the device rolled it
   back, and a retry made a new request that the contact refused; the
   contact's answer then named the first one and was dropped. Now the
   client tells a failure before sending (no connection:
   `RelayError::Unreachable`, rolled back as before) from one after sending
   (connection broke, timeout): then the change stays and the envelopes wait
   in `Device::unsent`, sent again unchanged by the next fetch (the
   recipient drops a copy by its identifier). Later messages to the same
   contact queue behind them. Dropped once older than a recipient accepts.
2. **Lost answer to `hello`.** The inviter used up the invitation for it,
   while the accepting side rolled back and waited for nothing. Now accept
   keeps the contact and its mailbox on such a failure; the existing
   repetition of `hello` gets the same `reveal`.
3. **Clock off by 15 minutes was silent.** The sender saw "sent", the
   recipient dropped the message, visible only with `--verbose`. Now the
   device with the wrong clock warns (decision 3), and the recipient gets a
   visible notice naming the contact.
4. **A removed device keeps the contact keys** (known, `protocol.md`). A copy
   of its state can still read and answer on the old mailboxes until the
   contacts are paired again. Decided on 2026-11-19 to leave this as
   documented; re-keying contacts when a device is removed is a later
   protocol change. (The first version of this record said new pairings
   were out of its reach; the security review showed they were not while
   the group stayed, see below.)

Also added (decided on 2026-11-19): `lz access-token` and Devices → "Change
access token" set a new access token after the relay's was replaced; the
relay must accept it (checked with a voucher). On a device without a relay
account the app needs a setup code, `lz` the option `--relay`.

## Security review (2026-11-19)

Done by a separate AI session (Opus) on the code of this branch, read-only,
with the brief in `review-brief.md`. Fixed the same day, all on this branch:

| # | Finding | Fix |
|---|---|---|
| High | A relay could hand a message back to its sender, where it opened as the contact's (`crypto_box` has one key for both directions): fake requests, an own answer shown as the contact's sign of life | direction marker in the last plaintext byte of form A; a device refuses its own; `0` from older clients still accepted (`wire-format.md`) |
| High | A removed device kept the group key and the sync mailbox tokens: it could register again, receive contacts paired later and send forged sync messages (e.g. `device_removed` for the phone) | removing ends the group: the sync mailbox is deleted; the other devices notice, keep their contacts and are added again; the removed one still deletes its state |
| Low | The 10 minutes of an invitation rested on the sender's time: an unused code could be accepted with a backdated `hello` for a day | a `hello` is taken at most one hour after the expiry |
| Low | One contact's unreachable relay held back resending to all contacts; permanent refusals were retried silently | per contact; permanent refusals dropped with a notice |
| Low | `http://` relay addresses from invitations sent tokens unencrypted and could reach the local network | plain `http` only to this computer (loopback), for local tests |
| Low | Invited people could fill all handover slots of the relay | quota per family (owner, each voucher family); guests together at most half; derived mailboxes at most 30 over their lifetime (schema version 2) |
| Low | Wake signals to any public address and port | port 443 only; the guide recommends `LZ_PUSH_ALLOWED_HOSTS` |
| Info | Messages of 1–512 bytes accepted | exactly one envelope (297 bytes) |
| Info | `lz-server` wrote its token to the log | the log says where it is |
| Info | The device code could be squatted if sent as text | the guide says to show it only |

Not changed: WebSocket connections without subscriptions have no idle limit
and the relay sets no connection cap of its own; the reverse proxy limits
both (timeouts in the examples). Checked and found sound: see the review
(envelopes and parsing, replay protection, pairing commitment, handover,
relay tokens, push address checks including DNS rebinding, local
interface, resending, `set_access_token`).

## Work

- Relay image job in `container.yml`.
- `deploy/relay/` and `docs/self-hosting.md`.
- Clock offset warning in `lz-client`, app banner, `lz` output, texts in both
  languages.
- Behaviour tests and fixes.
- Review brief, Opus review, fixes.
- Dry run of the guide, then the test record.

## Test record (2026-11-19)

Release APK and Windows installer from this branch, the Linux desktop app,
the Home Assistant add-on, `lz` as a test contact.

- [x] Guide dry run by a fresh session (local Docker, no proxy, no phone):
      relay and ntfy started, two `lz` devices paired with a voucher,
      request and answer both ways, a replaced token refused and set with
      `lz access-token`. Its findings are fixed in the guide.
- [x] Relay image `lebenszeichen-relay:edge` published for amd64 and arm64,
      pullable without login.
- [x] With the direction marker: request from the test contact answered on
      the phone, request from the phone answered by the test contact.
- [x] Phone clock 15 minutes ahead: the red banner appeared; its request
      was refused by the test contact with the visible notice naming the
      phone's person.
- [x] Devices → Change access token on the phone: accepted.
- [ ] Removing a device with a third device present (the remaining one
      leaves the ended group and keeps its contacts) – covered by tests.
- [ ] Done criterion: a person without prior knowledge sets up a relay and
      pairs two devices with the guide.
