# M10 – Satellite

Done when: an ESP32-C6 with an OLED and a button shows a request from the
test contact, a press sends a manual sign of life that the contact receives,
and the phone withdraws its display; a request answered on the phone turns
the satellite's light off; after the satellite is removed on the phone, it
deletes its state, and a copy of its keys can no longer read new requests
or answer them.

Builds on M5 (several devices, `wire-format.md`) and M9 (removing a device
ends the group, `m9-hardening.md`). This is "approach B" of the roadmap
("Microcontroller clients (later)").

## Decisions (2026-11-25)

1. **New device role: satellite.** It holds all contacts (contact keys,
   send tokens, its own reader token per mailbox), like other devices. It
   does **not** get the group key, the sync mailbox or the relay access
   token. It talks to the person's other devices only through a **narrow
   channel** over the relay. It cannot add or remove devices, change rules,
   pair or end contacts, or pose as a full device.
2. **A read token per reader on the relay**, for all devices, not only
   satellites. Removing a device revokes its token, so it can no longer read
   the contact mailboxes, without its cooperation. A response must name the
   16-byte random identifier of the request; without read access a removed
   device cannot learn it, so it cannot forge a sign of life to new
   requests.
3. **Self-deletion on removal**, as all devices do since M9. **No**
   self-wipe after some days offline.
4. **Secure boot and flash encryption** on the ESP32-C6 are the recommended
   setup.
5. **Setup and pairing of the satellite via BLE**, to be decided later. Not
   designed here; constraints are listed under open points.
6. **What the satellite does besides answering** (revised 2026-11-25):
   - A button press is a full manual sign of life: sent at once, kind
     `manual`.
   - It can hold a **reported state** (reason and end), set locally by a
     long press (a preset such as "asleep", 8 hours) or later by a sensor
     through the ESPHome component. It answers with kind `reported` under
     the same rules as every device: only after its waiting time, only to
     contacts that allow reported answers on it, at most one per request.
   - It **shares states both ways** as in M8c: its shared state reaches the
     full devices, and theirs (for example "Phone empty") reaches it.
   - It can **ask**: an ask button bound to one contact ("Is Mum okay?")
     sends a `request` under the contact's rules, and the OLED and light
     show the answer.
   - Its settings for this (reported answers per contact, waiting time,
     long-press preset, ask buttons) are set on a full device and sent
     through the channel. It cannot change them itself, nor any rules.
7. **Several open requests, one button:** one press answers all open
   requests shown; a second button, if present, only scrolls through them.
8. **The name of the asking contact is shown on the OLED**; the satellite is
   at home. A setting can come later.
9. **At most 3 relays per satellite** on the ESP32-C6; the phone refuses to
   add a satellite beyond that.
10. **Before BLE exists**, a satellite is set up over USB serial from `lz`,
    for development only and not shipped.
11. **Buttons:** the first board has the answer button and one ask button;
    the long press for the preset is on the answer button. More ask buttons
    are a firmware build option, bound to contacts by
    `home_contact_settings`.
12. **One preset in M10.** The ESPHome component sets reason, duration and
    share directly through the library, without a preset. A list of presets
    can come later in `home_settings` without a new type.
13. **The answer to an own request** is shown on the OLED, with the light in
    its colour, until the next press or 24 hours; dismissing it sends
    nothing, and a press while a request is open still answers that request.
14. **Setup via BLE, from the Android app only in M10** (desktop BLE later;
    `lz` over USB serial stays for development). Our own Lebenszeichen GATT
    service, the same on every SDK, not Espressif's provisioning (ESP-IDF
    only). The satellite is in setup mode when it has no state or when the
    button is held at power-on. Both sides exchange fresh X25519 keys over
    BLE; a 6-digit check code from both keys and a commitment, as in
    contact pairing, is shown on the OLED and on the phone and confirmed on
    the phone. Over that encrypted link the phone hands over WiFi name and
    password, the channel (relay, channel mailbox, enrolment token, send
    token, channel key) **and all contact records**; the channel carries only
    later changes. The satellite draws its own reader identifier and
    replaces the enrolment token at once.
15. **Changing WiFi**: setup mode again from the phone; keys and contacts
    stay. **Factory reset**: the button held for about 10 seconds at
    power-on wipes everything, as a removal does. Boards without a display
    (a printed fixed code) come later.
16. **C library**: one pure C99 core (`lz_core`: envelopes, crypto, message
    types, rules, satellite state; no network, no flash, no `malloc`; time,
    randomness and storage through callbacks) and thin adapters per SDK for
    network, storage and BLE. Crypto from libsodium where the SDK ships it
    (ESP-IDF, Arduino-ESP32), otherwise the bundled TweetNaCl; SHA-256 from
    mbedTLS. API around `lz_poll()` with events (request opened, request
    closed, answer received) and actions (answer, ask, set state). The Rust
    crates generate test vectors that the C tests in CI reproduce byte for
    byte.
17. **Order of the SDKs**: ESP-IDF with the ESP32-C6 firmware first, then
    ESPHome, Arduino, Pico SDK, MicroPython and CircuitPython. The code
    lives under `firmware/` in this repository; a CI job mirrors it into a
    separate repository `lebenszeichen-c` for the Arduino Library Manager,
    PlatformIO and ESPHome.
18. **License of the library**: LGPL-3.0-or-later with a static linking
    exception (draft in `license-exception.md`, legal review open).

## Threat model

The asset is not personal data (there is little: contact names, rules,
short reasons) but the ability to send fake "I'm fine" signs of life as the
person. The case to cover is a satellite that is lost or taken from the
hallway: once the person removes it on the phone, it must not be able to
answer new requests. Professional attackers who extract keys before the
person notices are out of scope. Before removal, a stolen satellite can do
what the satellite can do anyway: answer, ask, and share a state, so that
the full devices answer with its reason. A reported answer is always shown
as reported, so this is not a fake "✓". Removal is the remedy.

## Part 1: read token per reader

### Today

Every mailbox has one read token, shared by all devices of the person. It
authorizes subscribing, registering and unregistering readers, deleting the
mailbox, deriving mailboxes (voucher mailboxes) and storing handover slots.
A removed device keeps it, so it can register itself again and read on
(`m9-hardening.md`, gap 4).

### Relay

Schema version 3:

```sql
ALTER TABLE readers ADD COLUMN token_hash BLOB;            -- NULL: legacy reader
ALTER TABLE readers ADD COLUMN role INTEGER NOT NULL DEFAULT 1; -- 1 full, 2 satellite
CREATE INDEX readers_token_hash ON readers(token_hash);
```

`mailboxes.read_token_hash` becomes the **shared token**, and may be empty
(table rebuild in the migration). Existing mailboxes keep it; existing
readers start as legacy readers without a token of their own.

A bearer token for a mailbox is matched against the shared token, if there
is one, and against the `token_hash` of every reader of that mailbox. The
match decides the rights:

| Token | Subscribe, ack | Change own push address, own token | Register, change, revoke other readers | Delete mailbox, drop shared token, set send token | Revoke all readers but a given set | Derive mailboxes, store handover slots |
|---|---|---|---|---|---|---|
| Shared token | as a legacy reader | yes, for legacy readers | yes | delete only | no | yes |
| Full reader | yes | yes | yes | yes | yes | yes |
| Satellite reader | yes | yes | no | no | no | no |

A reader with a token of its own subscribes only with that token; the
shared token no longer works for it. Satellite tokens never authorize
anything outside their mailbox.

API changes, all under `/v1`:

| Call | Change |
|---|---|
| `GET /health` | `200 {"features":["reader_tokens"]}`. Old relays answer with an empty body. |
| `POST /mailboxes` | New form `{"reader":"<b64url>","reader_token_hash":"<hex>","send_token_hash":"<hex>"}`: no shared token, the reader is the first full reader. The old form stays. |
| `PUT /mailboxes/{m}/readers/{r}` | Body gains `"token_hash":"<hex>"` and `"role":"full"\|"satellite"`, both optional. A new reader on a mailbox without shared token needs `token_hash`. Only a full reader or the shared token may set `role` or touch another reader; a reader with its own token may change its own `push_url` and `token_hash`. |
| `DELETE /mailboxes/{m}/readers/{r}` | Also allowed with the reader's own token (leaving). |
| `DELETE /mailboxes/{m}/read-token` | New, full reader only: drops the shared token. Legacy readers lose access. `204`, also if there was none. |
| `PUT /mailboxes/{m}/send-token` | New, full reader only: `{"send_token_hash":"<hex>"}` replaces the send token. Used for the satellite channel. |
| `POST /mailboxes/{m}/revoke-readers` | New, full reader only (its own token): `{"keep":["<b64url>",…],"drop_shared_token":true\|false}`. Revokes every reader except the caller and those in `keep`, and drops the shared token if asked, in one transaction. `200 {"revoked":["<b64url>",…]}`. Part of `reader_tokens`. |
| WebSocket `subscribe` | Frame unchanged. Order of checks: mailbox (`unknown_mailbox`), **then reader** (`unknown_reader`), then token (`unauthorized`). Today the token comes before the reader. |

- The new order lets a revoked device learn that it was removed: revoking
  deletes the reader with its token, so it gets `unknown_reader` as today.
  Probing it needs both the 32-byte mailbox identifier and the 16-byte
  reader identifier.
- Revoking a reader with a live subscription ends it at once
  (`ReaderRemoved`, as today).
- Limits stay: 16 readers per mailbox, tokens of exactly 32 bytes, only
  hashes stored. Voucher families and handover quotas are found through the
  reader's mailbox as they are now through the shared token.
- The relay still does not know which readers belong to one person; the
  reader identifier already links mailboxes of one device today.

Settled in the implementation (relay, 2026-11-25):

- `role` in the schema is the role of a reader with a token of its own; a
  legacy reader subscribes with the shared token whatever its role. A new
  reader without `role` is a full reader. A stored role other than 1 counts
  as satellite.
- A satellite may not send `role` at all, not even its own. A token hash
  that the shared token or another reader of the same mailbox already has
  is refused (`400`), so a token always names one principal.
- In `PUT readers/{r}` a missing field stays as it is; `"push_url":null`
  still removes the push address. Old clients always send `push_url`.
- Changing a reader's token or dropping the shared token ends live
  subscriptions opened with the old token, with `unauthorized`; every
  delivery and acknowledgement checks the token again. A device that
  replaces its enrolment token while subscribed subscribes again with the
  new one.
- Dropping the shared token keeps the legacy readers registered (messages
  wait for their acknowledgement up to 24 hours); a full reader revokes them.
- Authorization and the change it allows run in one transaction, so a
  reader revoked at the same moment cannot register itself again.
- `DELETE readers/{r}` and the other calls keep their old order: wrong
  token before unknown reader. Only `subscribe` changed.
- The shared token is not really weaker than a full reader: it can register
  a full reader with a token of its choice. The table limits what it does
  directly; the protection comes from dropping it.
- **Hidden readers.** Before removal, R (with the shared token or its own
  full token) can register extra readers with tokens of its choice; the
  relay has no list of readers. Revoking only R's reader would leave them.
  Hence `revoke-readers`: E names the readers it knows to keep, and every
  other reader goes, together with the shared token, so that nothing
  planted survives and the shared token cannot plant a new one in between.
  The caller is always kept, whether it names itself or not: the call can
  never leave a mailbox without the full reader that made it, and a device
  that wants to leave uses `DELETE readers/{own}`. `keep` holds at most 16
  identifiers; unknown ones are ignored. Revoked readers' live
  subscriptions end with `unknown_reader`, and waiting messages no longer
  wait for them. The shared token and satellites may not call it.

### Client

- **Own token.** On a relay with `reader_tokens`, a device gives itself a
  random token per mailbox: `PUT readers/{own}` with the shared token and
  `token_hash`. From then on it uses its own token for that mailbox. New
  mailboxes are created in the new form.
- **Enrolment tokens.** A device that registers another reader (adding a
  device, a contact paired later, the satellite) gives it a random
  enrolment token. The receiver replaces it with its own token at once
  (`PUT readers/{own}` with `token_hash`), so that only it knows its token.
  In the handover, the `read_token` of each contact and of the group become
  enrolment tokens. In `contact_part` the field "read token of the own
  mailbox" becomes the enrolment token for all other devices of the group.
- **Removing a device** (extends `wire-format.md`, "Removing a device"):
  on every own mailbox E calls `revoke-readers` with
  `"drop_shared_token":true` and as `keep` the readers of the devices that
  stay (the other full devices and the satellite), so that R and any reader
  R planted go in one step; it deletes the sync mailbox (the group ends, as
  since M9), and rotates the send token of the satellite channel if there
  is one (below). Remaining devices are added again as since M9 and get
  fresh enrolment tokens with the handover.
- **Detection** stays: sync mailbox `unknown_mailbox` plus `unknown_reader`
  on an own contact mailbox means "this device was removed", and it deletes
  its state.

### Compatibility

- **New relay, old clients.** The shared token keeps working for them until
  a device drops it. An old client that receives an enrolment token in
  `contact_part` uses it as its read token; the relay matches it as that
  reader's token, so subscribing, its push address, deleting the mailbox
  and handover slots keep working. It just never replaces it.
- **Old relay, new clients.** No `reader_tokens` in `/health`: the client
  stays with the shared token. Removal stays as weak as today, and no
  satellite can read mailboxes on that relay.
- **Mixed relays.** An own mailbox can live on a contact's relay (accepted
  with the contact's voucher). On such a relay without `reader_tokens`, that
  contact gets the old behaviour, and the satellite does not get it (the
  phone says so).
- **Removing with old own devices.** Dropping the shared token locks out own
  devices that were not updated. They have to be added again after a
  removal anyway (M9); the app says to update them first.

Settled in the implementation (client, 2026-11-25):

- **Contact mailboxes keep the old form**, also on relays with
  `reader_tokens`: `contact_part` carries one token for all other devices,
  and the relay refuses a token hash that another reader of the mailbox
  has, so the shared token is that enrolment token. The device that
  creates the mailbox registers itself with its own token in the same
  `PUT`. Only the sync mailbox is created in the new form; its
  `read_token` in the state file is then the own token, which is never
  handed over. The shared token on contact mailboxes stays until a removal
  drops it (open point 1).
- **Receivers register themselves** from `contact_part`, as before, with the
  shared token and `token_hash` in one `PUT`, instead of D registering them.
- **Handover:** E registers N on every mailbox with a fresh random
  enrolment token per mailbox and hands those over in place of its own
  tokens; the core leaves own tokens out of every handover. N replaces them
  while joining; what fails there is retried by every fetch.
- **Migration:** every fetch (and every tick of `listen`) gives the device
  its own token on each own mailbox that has none, where the relay lists
  `reader_tokens`; idempotent. The new token is stored (`new_token`) before
  it is sent; if the token held so far is refused, the new one is tried, as
  the relay may have taken it with a lost answer. A refusal of both is
  remembered for the process. The state file format is 2; files of format 1
  are read and written back as 2, so that an older version does not open a
  file whose own tokens it would ignore.
- **Removing:** `revoke-readers` on every own contact mailbox that has not
  ended (not on open invitations: the removed device never knew their
  tokens), with `keep` the other known devices (satellites are added in
  part 2) and `"drop_shared_token":true`; then the sync mailbox is deleted.
  On a relay without `reader_tokens` the reader is only unregistered, and
  the device says for which contacts removing is weak. It also reports how
  many unknown readers were revoked (planted by the removed device, or left
  by devices removed earlier) and which contacts had a request open.
- **`unauthorized`** on an own mailbox: if the device replaced its token
  meanwhile, it subscribes again (expected, not a removal); a reader
  without its own token tries to take one; otherwise its access was
  withdrawn (the shared token was dropped while it was a legacy reader),
  and it says to set it up again.
- **Residual:** until N replaces its enrolment tokens, E knows them; a
  device before M10 never replaces them, so the device that enrolled it
  could act as it after being removed itself. Update old devices.

### What a removed device can still do

After E revoked it on relays with `reader_tokens`:

- It cannot read anything new on the own mailboxes, nor register itself
  again; readers it planted before the removal are revoked with it
  (`revoke-readers`).
- **Requests open at the moment of removal**: it knows their identifiers
  and can answer them until they expire (default 24 hours) or the person
  answers them manually; after a manual response the requester accepts no
  further one. The app suggests answering open requests after removing a
  lost device.
- With the send tokens, which contacts issued and only contacts can change:
  `request` within the contact's rules (the person's devices enforce them),
  `rules` (it could, for example, set the weekly limit to 0 and so grey out
  the contact's button), and `end` (visible on both sides). It cannot send a
  valid response to a request it has not seen.
- With the contact keys: decrypt ciphertext of these contacts that it
  recorded before.
- A full device may also hold the relay access token; the person then
  changes it (`lz access-token`). A satellite never holds it.

Pairing the contacts again fixes all of it, as before. Removal is now enough
against fake signs of life; re-pairing is for the rest.

## Part 2: the satellite

### What it does

- Reads every own contact mailbox, with its own satellite-role token.
- Applies the rules for requests (`wire-format.md`, "Rules for requests")
  with the own rules per contact, so it shows exactly what the full devices
  show. Counters come from the requests it reads, as on every device.
- Shows open requests on the OLED with the contact's name, and a light while
  one is open. A press answers: `response`, kind `manual`, to the contact's
  mailbox, then `sat_answered` on the channel. Without network it keeps both
  and sends them later with the time of the press (valid as long as the
  request had not expired then).
- **Reported answers.** Two state slots as in M8c: the own state (long
  press, sensor) and the handed state (from a full device); the valid one
  set last applies. When the waiting time after an open request has passed,
  a state is valid, the contact allows reported answers on the satellite
  and no reported answer to this request was sent yet, here or elsewhere,
  it sends `response`, kind `reported`, and `sat_answered` with that kind.
  The request stays open, as everywhere. Another long press, or the end of
  the time, ends the own state.
- **Sharing.** If the preset says so (`home_settings`), a long press also
  shares the state (`sat_state_shared`), and ending it sends
  `sat_state_cleared`. Received `home_state_shared` and
  `home_state_cleared` act as `state_shared` and `state_cleared` in M8c:
  newest wins by sending time and identifier, a clear ends the handed state
  and a shared own state, an own state that was never shared stays.
- **Asking.** An ask button checks the requester's side of the rules
  (`wire-format.md`, "Rules for requests") with the contact's rules for the
  person: an own open request, the minimum interval since the contact's
  last manual answer to an own request, the weekly limit over own requests.
  If allowed, it sends `request` to the contact's mailbox and `sat_asked`
  on the channel; otherwise the OLED shows "possible again from 18:00". It
  learns requests sent from full devices through `home_requested`. The
  contact's `response` arrives in the own mailbox, which the satellite
  reads: the OLED shows "✓ Mum 14:02" or the reported reason, and the light
  changes colour until the next press. It needs the contact's `rules` for
  this; it reads them in the own mailbox itself (last writer wins), and
  the contact record carries the current ones.
- `end` from a contact marks the contact ended and turns its light off.
  Other types in contact mailboxes are ignored.
- Writes and checks the direction marker of form A (it has both public
  keys), and keeps replay protection per contact and for the channel as in
  `wire-format.md`, persisted in flash, so that a restart does not reopen
  old requests.
- Keeps its WebSocket connection open itself and has no push address. It
  pings to keep the reverse proxy from closing it, and reconnects with
  backoff.

### The channel

One **channel mailbox** per satellite, on the relay of the full device that
set it up, and one **channel key** (32 bytes, symmetric). Envelopes are form
C (`crypto_secretbox`), with the plaintext header of `wire-format.md`.

- **Readers:** the satellite (role `satellite`) and every full device (role
  `full`). Full devices register a push address, so the satellite's answer
  wakes the phone. The satellite has none.
- **Send token:** one, held by the satellite and the full devices.
- **Replay protection:** one set for the channel on each side, same window
  as for contacts.
- **Types are one-way.** Up types (80–87, `sat_*`) go from the satellite to
  the full devices, down types (88–99, `home_*`) from the full devices to
  the satellite. Full devices accept only up types, the satellite only down
  types. Anything else that opens with the channel key is dropped silently,
  as is anything that does not open. A reflected message is therefore
  always of a type the receiver drops; no direction marker is needed.
- Up types carry only what the satellite did (answered, asked) and its
  shared state. A full device takes nothing else from the satellite: no
  rules, no contacts, no devices, no settings.

Messages (the range 80–99 is for the channel, only in form C, only in a
channel mailbox; types not listed are reserved):

| Type | Name | Direction | Body |
|---|---|---|---|
| 80 | `sat_answered` | up | as `responded` (67): contact identifier (32), identifier of the request (16), kind (1), length of the reason (1), reason (at most 64), time of the response (8) |
| 81 | `sat_asked` | up | as `requested` (66): contact identifier (32), identifier of the request (16), time of the request (8) |
| 82 | `sat_state_shared` | up | as `state_shared` (71): length of the reason (1), reason (at most 64), end of the state (8) |
| 83 | `sat_state_cleared` | up | – |
| 88 | `home_contact_part` | down | as `contact_part` (69): contact identifier (32), transfer identifier (16), part (1), number of parts (1), length (1), data (at most 178) |
| 89 | `home_responded` | down | as `responded` (67) |
| 90 | `home_requested` | down | as `requested` (66) |
| 91 | `home_rules` | down | contact identifier (32), own rules for this contact as in `rules` (10) |
| 92 | `home_contact_settings` | down | contact identifier (32), reported answers allowed on the satellite (1: `0` no, `1` yes), ask button (1: `0` none, otherwise its number) |
| 93 | `home_settings` | down | waiting time in minutes (2), preset: duration in minutes (4), share (1: `0` no, `1` yes), length of the reason (1), reason (at most 64) |
| 94 | `home_state_shared` | down | as `state_shared` (71) |
| 95 | `home_state_cleared` | down | – |
| 96 | `home_send_token` | down | new send token of the channel (32) |

- `sat_answered`: full devices apply it like `responded` with its kind: a
  manual one withdraws the display and starts the minimum interval, a
  reported one keeps them from sending a second reported answer. Parked for
  24 hours if the contact or request is not known yet, as for sync
  messages. Not passed on.
- `sat_asked`: applied like `requested`: the button is disabled on the full
  devices, and they accept the contact's response to it.
- `sat_state_shared`, `sat_state_cleared`: applied like `state_shared` and
  `state_cleared` from another full device (M8c), with the same stamps;
  each full device reads the channel itself, so none passes them on.
- `home_contact_part`: a contact paired later, or all contacts at setup. The
  parts form the **satellite contact record** (at most 698 bytes, 4 parts):

  | Length | Content |
  |---|---|
  | 32 | Secret contact key |
  | 32 | Public contact key of the contact |
  | 32 | Enrolment token of the satellite on the own mailbox |
  | 1 + at most 120 | Own relay address |
  | 32 | Mailbox identifier of the contact |
  | 32 | Send token for it |
  | 1 + at most 120 | Relay address of the contact |
  | 10 + 8 + 16 | Own rules for this contact, with sending time and message identifier of the last applied ones |
  | 10 + 8 + 16 | Rules of the contact, with their stamp |
  | 1 + 1 | Contact settings of the satellite, as in `home_contact_settings` |
  | 16 + 8 | Open own request: identifier and time, zero bytes if none |
  | 8 | Time of the contact's last manual answer to an own request, 0 if none |
  | 1 + 8 × at most 16 | Times of the own requests to this contact in the last 7 days (number, times) |
  | 1 + at most 64 | Local name of the contact |

  Same checks for parts as for `contact_part`; 4 parts are within what
  `contact_part` accepts. The satellite stores the contact, replaces the
  enrolment token with its own and subscribes. A record for a contact it
  has already replaces the name, and rules and settings whose stamp is
  newer, so that a renamed contact can be sent again. With more than 16
  own requests in a week the oldest are left out; the satellite may then
  allow a request that the contact discards, and the contact's check stays
  authoritative, as on every device.
- `home_responded`: another device answered. Manual: the satellite turns
  the light off and starts the minimum interval. Reported: it sends no
  reported answer of its own to that request; the light stays on.
- `home_requested`: a full device asked the contact; the satellite counts
  it, disables the ask button and accepts the response.
- `home_rules`: sent with the same time and identifier as the `rules` to
  the contact, applied only if newer (last writer wins, as `rules_set`).
- `home_contact_settings`, `home_settings`: applied only if newer, by
  sending time and identifier. Defaults until one arrives: reported answers
  off for every contact (as `concept.md`), waiting time 15 minutes, no
  preset, no ask buttons.
- `home_state_shared`, `home_state_cleared`: sent with the same time and
  identifier as the matching sync message.
- `home_send_token`: after a full device was removed (below). The satellite
  sends with the new token from then on.

Every full device sends its own `home_*` next to the matching sync message:
`home_responded` with `responded` (both kinds), `home_requested` with
`requested`, `home_rules` with `rules_set`, `home_state_shared` and
`home_state_cleared` with theirs, `home_contact_part` with `contact_part`
(after registering the satellite's reader with role `satellite` and an
enrolment token). Retries use the outbox, as for the sync mailbox.

**Not on the channel, and why:**

- *"Request seen"*: nothing on the full devices would change, and the
  concept has no read receipts.
- *Contact removed*: the device that ends a contact deletes its mailbox; the
  satellite gets `unknown_mailbox` for it and deletes the contact. Same for
  the 90-day cleanup.
- *Rules of the contact*: the satellite reads them in the own mailbox.
- *The satellite's own settings, up*: full devices learn them from each
  other (below), never from the satellite.
- *Device list, group changes*: the satellite does not need them.

### The satellite among the full devices

Full devices learn about a satellite through the sync mailbox:

| Type | Name | Body |
|---|---|---|
| 73 | `satellite_part` | reader identifier of the satellite (16), transfer identifier (16), part (1), number of parts (1), length (1), data (at most 178) |

The record (at most 282 bytes, 2 parts): name (1 + at most 32), relay of
the channel (1 + at most 120), channel mailbox (32), enrolment token of the
recipients on it (32: the channel's shared token, see below), send token
(32), channel key (32). Checks as for `contact_part`. The handover gets a `satellites` list with the same
content, the satellite's settings and the channel's replay protection.
Older devices ignore types 73 to 75.

The satellite's settings are kept on every full device, so that any of them
can show and change them. The device that changes them sends `home_*` to
the channel and tells the other full devices through the sync mailbox,
applied only if newer:

| Type | Name | Body |
|---|---|---|
| 74 | `satellite_settings` | reader identifier of the satellite (16), then as `home_settings` |
| 75 | `satellite_contact_settings` | reader identifier of the satellite (16), then as `home_contact_settings` |

The satellite shows up in the device list as a satellite. Any full device
can remove it.

### Removing the satellite

1. A full device E revokes the satellite's reader on every own mailbox and
   on the channel mailbox, then deletes the channel mailbox.
2. The group stays: the satellite never had the group key or the sync
   mailbox. The other full devices see `unknown_mailbox` on the channel and
   forget the satellite.
3. The satellite, when it next connects, gets `unknown_reader` or
   `unknown_mailbox` for the channel mailbox, deletes its whole state
   (erases the key partition) and shows "removed". Unlike full devices, a
   satellite treats the channel alone as decisive, because removing a
   satellite is the only way its channel mailbox goes away.
4. A stolen satellite that stays offline keeps its keys, but with them it
   cannot read new requests anywhere; see "What a removed device can still
   do". Asking and reporting add nothing there: with the channel deleted it
   can no longer share a state with the full devices, and the answers to
   its own requests arrive in the own mailboxes, which it cannot read.

### Removing a full device while there is a satellite

The removed device R knows the channel key and send token. E:

1. revokes R on the channel mailbox with `revoke-readers` (keeping the
   satellite and the other full devices), so that neither R nor a reader it
   planted can read there;
2. sets a new send token (`PUT /send-token`) and sends it to the satellite
   in `home_send_token`, so that R cannot fill the channel or feed the
   satellite forged rules, settings, contacts, states or "answered
   elsewhere";
3. keeps the channel key: R can neither read nor send on the channel any
   more, and the key alone opens only what R saw before.

The group ends as since M9; the satellite stays linked. Remaining full
devices still read the channel (their reader tokens are untouched) but
cannot send to it until they are added again; the handover then carries
the new send token and an enrolment token.

### Contacts paired later

The pairing full device D creates the own mailbox, registers the other full
devices and the satellite (role `satellite`) with enrolment tokens, sends
`contact_part` to the sync mailbox and `home_contact_part` to the channel.
If D was offline from the group (after a removal), the satellite gets the
contact once D is added again, like the other full devices.

### Storage on the device

Secure boot and flash encryption (with NVS encryption) are the recommended
setup on the ESP32-C6. On platforms without flash encryption, such as the
RP2040, keys are in plain flash; removal still locks a stolen one out.

Kept in flash, so that a restart changes nothing a contact or the person
could notice: contacts with replay protection and counters; incoming
requests of the last 7 days (identifier, time, answered manually, reported
answer sent); own requests of the last 7 days and the open one; both state
slots with their stamps; settings with their stamps; the outbox. Waiting
time timers are not stored: after a restart they follow from the times of
the open requests and the waiting time. Writes are small and rare (a few
per request), well within the wear of NVS.

### What the C library must implement

Without the API, which is left for later:

- `crypto_box_easy` and `crypto_box_open_easy` (X25519, XSalsa20,
  Poly1305), `crypto_secretbox_easy` and `crypto_secretbox_open_easy`,
  SHA-256, a hardware random source. `crypto_box_seal` is not needed by the
  satellite unless setup uses it. libsodium (an ESP-IDF component) or
  TweetNaCl fit; Monocypher does not (XChaCha20 instead of XSalsa20).
- Envelopes of exactly 297 bytes, the plaintext header, the bodies of
  types 3–6 and 80–96, the direction marker, padding with zero bytes.
- Rules for requests on both sides (accepting a request, and checking an
  own one with the contact's rules), replay protection, the satellite
  contact record.
- Reported answers: two state slots, newest-wins stamps, waiting time
  timers, one reported answer per request.
- Relay access: HTTPS with certificate check (a CA bundle), WebSocket with
  the JSON frames of `wire-format.md`, b64url and hex.
- Time from NTP, checked against the relay's `Date` header; above 5 minutes
  apart it shows a warning and sends nothing (M9, decision 3).
- Persistent storage behind a small interface (contacts, replay
  protection, outbox), and erasing it.

### Settled in the implementation (full devices, 2026-11-26)

Step 2b: wire types, the full-device side in `lz-core` and `lz-client`,
`lz satellite`, and a reference satellite for the tests
(`crates/client/src/tests/reference.rs`). Byte layouts are in
`wire-format.md`, "Satellite (M10)".

Fixes of this design:

- **One enrolment token for all full devices.** `satellite_part` goes to
  every full device, but the relay refuses a token hash that another
  reader has, so "the enrolment token of the recipient" cannot be one per
  recipient. The channel is created in the old form, as contact mailboxes
  are: its shared token is the enrolment token in the record, every full
  device registers itself with it and a token of its own in one `PUT`, and
  the device that set it up does so at once. The satellite never sees the
  shared token; it gets an enrolment token of its own. The shared token
  goes at the next removal of a full device (`revoke-readers` with
  `drop_shared_token`), as on contact mailboxes.
- **Settings in the satellite contact record carry no stamp**, yet
  `home_contact_settings` are applied only if newer. So the settings in the
  record are taken only for a new contact (with an empty stamp, which every
  `home_contact_settings` beats); a record for a known contact leaves them.
  Every change is sent down anyway, so the satellite ends with the newest.
- **`home_send_token`: the last one received wins**, not the newest stamp.
  With stamps, a device that is removed could plant one with a sending time
  up to 10 minutes ahead before its removal, and the satellite would ignore
  the real one. The relay keeps the order, and after the removal only the
  device that replaced the token on the relay can send on the channel.
- **The setup** (decision 14) had no byte layout: defined in
  `wire-format.md`, "Satellite setup", with the own mailbox identifier per
  contact, which the contact record leaves out.
- **No preset** is a duration of 0 in `home_settings`.
- **Sync types end at 79**: 80–99 is the channel, so `is_sync` covers 64–79
  and 100–127, and 71–127 is no longer "reserved for own devices" as a
  whole.

Interpretations:

- **3 relays** (decision 9) count the relays the satellite keeps a
  connection to: that of the channel and those of the own mailboxes it
  reads. Relays it only delivers to (the contacts') are not counted. Setup
  refuses more; a contact paired later that would need a fourth is left out
  with a notice, as is one whose relay lacks `reader_tokens`. The count for
  a later contact uses all own mailbox relays, so it may refuse where an
  exact count would not.
- **The waiting time on the satellite** runs from the time of the request
  (its sending time, capped at the own clock), not from its arrival as on
  full devices, so that it follows from the stored requests after a
  restart.
- **The times of own requests** in the record include the open request;
  the satellite rebuilds its outgoing ledger from them (counted entries
  without identifier, the open one with its identifier).
- **Incoming requests** of a contact are not in the record (there is no
  room in 4 parts). At setup the satellite counts only the requests it
  reads from then on: a request open at that moment is not shown on it,
  and for up to a week it may accept a request that the full devices
  discarded under the weekly limit. Only a contact that breaks the rules
  sends such a request.
- **Times in up messages** (`sat_answered`, `sat_asked`) are capped at the
  full device's clock.
- **An ask button names one contact**: binding it to a contact takes it
  from the one that had it (two `home_contact_settings`).
- **Down messages go to every satellite for every contact**: a full device
  does not know which contacts a satellite got (another device may have
  enrolled it). The satellite keeps those about unknown contacts for 24
  hours, then drops them.
- **Full devices do not remember down types** in the replay protection of
  the channel; they never apply one, own ones included.
- **Removing a satellite** deletes its reader on every own contact mailbox
  and on the channel, then the channel; it is forgotten only when all of
  that succeeded, so the command can be repeated. Other full devices
  forget it on `unknown_mailbox` (or `unknown_reader`) for the channel.
- **Removing a full device** does the channels first: `revoke-readers`
  keeping the other own devices and the satellites, dropping the shared
  token, then a new send token stored before it is sent
  (`new_send_token`), `PUT /send-token`, `home_send_token`. Other remaining
  full devices still read the channel; their deliveries there are refused
  until they are added again, and stay in the outbox for 24 hours.
- **The device list** shows satellites, marked as such; removing one by
  its reader identifier removes the satellite.
- **`lz satellite add`** takes the reader identifier the satellite drew
  (`--reader`) or draws one, and writes the setup to a file (mode 0600) or
  prints it as hex. WiFi name and password go into the setup as given.
- **State file**: `satellites` and `satellite_transfers` in format 2 (not
  released yet), left out when empty. Handover: `satellites`, left out when
  empty, without own tokens, outbox, parked messages and pending
  enrolments; E registers N on each channel with an enrolment token, which
  N replaces as for contact mailboxes.

Test vectors for the C library: `vectors()` in the reference satellite,
reproducible from a seed; `LZ_VECTORS_OUT=<file> cargo test -p lz-client
vectors_are_reproducible` writes them: every up and down type as plaintext
and envelope under a channel key, a satellite contact record, a setup, and
cases of the asking rule and of the waiting time.

### Settled in the implementation (C library, 2026-11-26)

Step 4a: `firmware/`, the portable C99 core `lz_core` without SDK adapters
(`firmware/README.md`). The Rust vectors gained form A both ways between
the satellite and a contact (`contact`, drawn after the others, which stay
as they were); the C tests reproduce all of them byte for byte.

- **Bounded tables** where the Rust model has vectors: 16 requests per
  contact and direction (the oldest answered one gives way; with a weekly
  limit above 16 the satellite may show a request the full devices discard,
  as the record already allows), 24 remembered identifiers per contact and
  64 for the channel, 16 outbox entries, 8 parked messages, 2 transfers. A
  full replay table forgets its oldest identifier and from then on refuses
  everything sent at or before it, so no replay gets through; a full outbox
  refuses the press or ask (event) instead of dropping something queued.
- **`home_contact_part` with more than 4 parts** is discarded: the
  satellite contact record never needs more, and the reassembly buffer is
  fixed.
- **Contact records** whose relay addresses are not `https://`, whose key
  is a low-order point (libsodium's check, also on TweetNaCl), or whose own
  mailbox is the channel's are refused, at setup as a whole.
- **Deliveries wait for the clock check**: nothing goes out before one
  relay `Date` header agreed with the own clock within 5 minutes; while it
  disagrees, messages are left on the relay unacknowledged and fetched
  again once it agrees. A `400` from the relay drops the envelope, other
  failures retry with backoff for 24 hours.
- **The own token is taken before subscribing**: a mailbox is subscribed
  once its enrolment token is replaced; if the replacement fails for good
  (`401` with both tokens, `404`), it is subscribed with the enrolment
  token so that the relay's answer (`unknown_reader`, `unknown_mailbox`)
  decides.
- **Removal** counts only for errors to a subscription the satellite made
  on the channel's connection.

## Open points

None. Decided on 2026-11-25: the shared token on contact mailboxes is
dropped only at a removal, not earlier. Readers planted before a removal
are caught by it (`revoke-readers`), which is enough.

## Work

- Wire: types 73–75, 80–83, 88–96, the satellite contact and satellite
  records.
- Relay: schema 3, token matching and roles, new calls, `/health`
  features, check order of `subscribe`, tests.
- Core and client: own tokens and enrolment, removal with revoke and
  dropping the shared token, satellite list and settings, channel send and
  receive, removing a satellite, `lz satellite` commands.
- App: satellite in the device list with its settings (reported answers
  per contact, waiting time, preset, ask buttons), remove, notices for
  relays without `reader_tokens`.
- Satellite firmware for the ESP32-C6, on the first cut of the C library.
- Docs: `wire-format.md`, `protocol.md`, `self-hosting.md` (lost device),
  `review-brief.md`, roadmap.

## Test plan

Items marked "model" are covered by the reference satellite against the
fake relay (`crates/client/src/tests/satellite.rs`, 2026-11-26); they stay
open until the ESP32-C6 passes them.

- [ ] Relay migration from schema 2: existing mailboxes and readers work
      with the shared token; old `lz` against the new relay passes its
      tests. (Relay side covered by `migration_from_version_2` and the
      existing relay tests, 2026-11-25.)
- [x] Updated devices switch to own tokens; the shared token then no
      longer subscribes as them.
      (Client tests, `crates/client/src/tests/tokens.rs`, 2026-11-25.)
- [x] An old device receives a contact paired on a new one and reads it
      with the enrolment token.
      (Client tests, `crates/client/src/tests/tokens.rs`, 2026-11-25.)
- [x] Removing a full device: its copy of the state gets `unknown_reader`
      and deletes itself; a saved copy can neither subscribe nor register
      itself again on any own mailbox; a new request is not answerable by
      it.
      (Client tests, `crates/client/src/tests/tokens.rs`, 2026-11-25.)
- [x] Removing on an old relay: the client says protection is limited.
      (Client tests, `crates/client/src/tests/tokens.rs`, 2026-11-25.)
- [x] A satellite token cannot register or revoke readers, delete the
      mailbox, drop the shared token, set the send token, derive mailboxes
      or store handover slots (all `401`).
      (Client tests against the fake relay, `the_satellite_cannot_act_as_a_full_device`,
      2026-11-26; relay side in `crates/relay/tests/relay.rs`.)
- [ ] Satellite shows a request from the test contact; a press sends a
      `manual` response; the phone withdraws its display.
      (model: `a_press_answers_every_open_request_and_the_phone_withdraws_its_own`)
- [ ] Request answered on the phone: the satellite's light goes off.
      (model: `an_answer_elsewhere_turns_the_light_off`)
- [ ] Rules changed on the phone reach the satellite; a request beyond the
      weekly limit is discarded by phone and satellite alike.
      (model: `rules_changed_on_the_phone_reach_the_satellite`)
- [ ] A contact paired on the phone appears on the satellite; a contact
      ended disappears from it.
      (model: `a_contact_paired_later_reaches_the_satellite`)
- [ ] Long press sets the preset; after the waiting time the test contact
      gets "“asleep” (reported)" from the satellite, only if allowed for it;
      a later press still sends ✓; the phone sends no second reported
      answer.
      (model: `reported_answers_after_the_waiting_time_only_where_allowed_once`)
- [ ] A shared state of the satellite is answered by the phone after its
      waiting time; "Phone empty" shared by the phone is answered by the
      satellite; a clear on either side ends it on both; an own unshared
      state stays.
      (model, without the answers: `states_are_shared_both_ways`)
- [ ] Ask button: request reaches the test contact, the phone disables its
      button, the answer shows on the OLED; a second ask within the minimum
      interval is refused on the satellite; a request sent from the phone
      disables the ask button.
      (model: `asking_obeys_the_rules_of_the_contact_and_the_phone_sees_it`, `requests_made_elsewhere_reach_the_satellite`)
- [ ] Settings changed on one full device reach the satellite and the
      other full devices.
      (model: `settings_propagate_between_full_devices_and_down`)
- [ ] A restart during the waiting time: the reported answer still goes out
      once, at the right time.
      (model: `a_restart_during_the_waiting_time_still_reports_once`)
- [ ] Full devices drop every down type and the satellite every up type and
      anything unknown; replays are dropped after a restart.
      (model: `the_satellite_cannot_act_as_a_full_device`, `the_satellite_drops_up_types_and_replays_after_a_restart`)
- [ ] Removing a full device while there is a satellite: the satellite stays
      linked and gets the new send token; the removed device's send to the
      channel is refused.
      (model: `removing_a_full_device_keeps_the_satellite_and_rotates_the_send_token`)
- [ ] Removing the satellite: it deletes its state on the next connection;
      the group stays; a dump of its flash cannot subscribe to any mailbox.
      (model: `removing_the_satellite_locks_it_out_and_it_wipes_itself`)
- [ ] Satellite clock 15 minutes off: warning on the OLED, nothing sent.
- [ ] Satellite offline, press, back online: the response arrives with the
      time of the press.
      (model: `a_press_offline_goes_out_later_with_the_time_of_the_press`)
