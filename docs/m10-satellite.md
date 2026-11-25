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
6. **A button press is a full manual sign of life**: sent at once, kind
   `manual`. The satellite never sends reported responses.
7. **Several open requests, one button:** one press answers all open
   requests shown; a second button, if present, only scrolls through them.
8. **The name of the asking contact is shown on the OLED**; the satellite is
   at home. A setting can come later.
9. **At most 3 relays per satellite** on the ESP32-C6; the phone refuses to
   add a satellite beyond that.
10. **Before BLE exists**, a satellite is set up over USB serial from `lz`,
    for development only and not shipped.

## Threat model

The asset is not personal data (there is little: contact names, rules,
short reasons) but the ability to send fake "I'm fine" signs of life as the
person. The case to cover is a satellite that is lost or taken from the
hallway: once the person removes it on the phone, it must not be able to
answer new requests. Professional attackers who extract keys before the
person notices are out of scope. Before removal, a stolen satellite can do
what the satellite can do anyway; removal is the remedy.

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

| Token | Subscribe, ack | Change own push address, own token | Register, change, revoke other readers | Delete mailbox, drop shared token, set send token | Derive mailboxes, store handover slots |
|---|---|---|---|---|---|
| Shared token | as a legacy reader | yes | yes | delete only | yes |
| Full reader | yes | yes | yes | yes | yes |
| Satellite reader | yes | yes | no | no | no |

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
  E revokes the reader of the removed device R on every own mailbox, drops
  the shared token there, deletes the sync mailbox (the group ends, as
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

### What a removed device can still do

After E revoked it on relays with `reader_tokens`:

- It cannot read anything new on the own mailboxes, nor register itself
  again.
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
- Ignores everything else in contact mailboxes: `response` from the contact
  (it has no own requests), `rules` from the contact, `end` marks the
  contact ended and turns its light off.
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
- **Types are one-way.** The satellite accepts only the types "to the
  satellite", full devices only the type "from the satellite". Anything
  else that opens with the channel key is dropped silently, as is anything
  that does not open. A reflected message is therefore always of a type the
  receiver drops; no direction marker is needed.
- The channel carries no state and no reports, and nothing a full device
  would take as an instruction other than "the satellite answered".

Messages (the range 80–89 is reserved for the channel, only in form C, only
in a channel mailbox):

| Type | Name | Direction | Body |
|---|---|---|---|
| 80 | `sat_answered` | satellite → full | contact identifier (32), identifier of the request (16), time of the response (8) |
| 81 | `sat_contact_part` | full → satellite | as `contact_part` (69): contact identifier (32), transfer identifier (16), part (1), number of parts (1), length (1), data (at most 178) |
| 82 | `sat_responded` | full → satellite | contact identifier (32), identifier of the request (16), time of the response (8) |
| 83 | `sat_rules` | full → satellite | contact identifier (32), own rules for this contact as in `rules` (10) |
| 84 | `sat_send_token` | full → satellite | new send token of the channel (32) |

- `sat_answered`: full devices apply it like `responded` with kind `manual`:
  withdraw the display, start the minimum interval from that time. Parked
  for 24 hours if the contact or request is not known yet, as for sync
  messages. Not passed on.
- `sat_contact_part`: a contact paired later, or all contacts at setup. The
  parts form the **satellite contact record** (at most 501 bytes, 3 parts):

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
  | 1 + at most 64 | Local name of the contact |

  Same checks for parts as for `contact_part`. The satellite stores the
  contact, replaces the enrolment token with its own and subscribes. A
  record for a contact it has already replaces the name and the rules if
  its rules stamp is newer, so that a renamed contact can be sent again.
- `sat_responded`: another device answered manually; the satellite turns
  the light off and starts the minimum interval. Reported responses are not
  sent, as they leave the request open.
- `sat_rules`: sent with the same time and identifier as the `rules` to the
  contact, applied only if newer (last writer wins, as `rules_set`).
- `sat_send_token`: after a full device was removed (below). The satellite
  sends with the new token from then on.

Every full device sends its own `sat_*` next to the matching sync message:
`sat_responded` with `responded`, `sat_rules` with `rules_set`,
`sat_contact_part` with `contact_part` (after registering the satellite's
reader with role `satellite` and an enrolment token). Retries use the
outbox, as for the sync mailbox.

**Not on the channel, and why:**

- *"Request seen"*: nothing on the full devices would change, and the
  concept has no read receipts.
- *Contact removed*: the device that ends a contact deletes its mailbox; the
  satellite gets `unknown_mailbox` for it and deletes the contact. Same for
  the 90-day cleanup.
- *Requests of the person's own*: the satellite does not ask.
- *States (M8, M8c)*: the satellite sends no reported responses.
- *Device list, group changes*: the satellite does not need them.

### The satellite among the full devices

Full devices learn about a satellite through the sync mailbox:

| Type | Name | Body |
|---|---|---|
| 73 | `satellite_part` | reader identifier of the satellite (16), transfer identifier (16), part (1), number of parts (1), length (1), data (at most 178) |

The record (at most 282 bytes, 2 parts): name (1 + at most 32), relay of
the channel (1 + at most 120), channel mailbox (32), enrolment token of the
recipient on it (32), send token (32), channel key (32). Checks as for
`contact_part`. The handover gets a `satellites` list with the same
content and the channel's replay protection. Older devices ignore type 73.

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
   do".

### Removing a full device while there is a satellite

The removed device R knows the channel key and send token. E:

1. revokes R on the channel mailbox, so it can read nothing there;
2. sets a new send token (`PUT /send-token`) and sends it to the satellite
   in `sat_send_token`, so that R cannot fill the channel or feed the
   satellite forged rules, contacts or "answered elsewhere";
3. keeps the channel key: R can neither read nor send on the channel any
   more, and the key alone opens only what R saw before.

The group ends as since M9; the satellite stays linked. Remaining full
devices still read the channel (their reader tokens are untouched) but
cannot send to it until they are added again; the handover then carries
the new send token and an enrolment token.

### Contacts paired later

The pairing full device D creates the own mailbox, registers the other full
devices and the satellite (role `satellite`) with enrolment tokens, sends
`contact_part` to the sync mailbox and `sat_contact_part` to the channel.
If D was offline from the group (after a removal), the satellite gets the
contact once D is added again, like the other full devices.

### Storage on the device

Secure boot and flash encryption (with NVS encryption) are the recommended
setup on the ESP32-C6. On platforms without flash encryption, such as the
RP2040, keys are in plain flash; removal still locks a stolen one out.

### What the C library must implement

Without the API, which is left for later:

- `crypto_box_easy` and `crypto_box_open_easy` (X25519, XSalsa20,
  Poly1305), `crypto_secretbox_easy` and `crypto_secretbox_open_easy`,
  SHA-256, a hardware random source. `crypto_box_seal` is not needed by the
  satellite unless setup uses it. libsodium (an ESP-IDF component) or
  TweetNaCl fit; Monocypher does not (XChaCha20 instead of XSalsa20).
- Envelopes of exactly 297 bytes, the plaintext header, the bodies of
  types 4, 5, 6 and 80–84, the direction marker, padding with zero bytes.
- Rules for requests, replay protection, the satellite contact record.
- Relay access: HTTPS with certificate check (a CA bundle), WebSocket with
  the JSON frames of `wire-format.md`, b64url and hex.
- Time from NTP, checked against the relay's `Date` header; above 5 minutes
  apart it shows a warning and sends nothing (M9, decision 3).
- Persistent storage behind a small interface (contacts, replay
  protection, outbox), and erasing it.

## Open points

1. **Setup and pairing via BLE, to be decided.** Constraints from this
   design: setup is started on a full device and needs the relay with
   `reader_tokens`; it must hand the satellite the channel (relay, channel
   mailbox, enrolment token, send token, channel key) and WiFi credentials,
   over an authenticated BLE link (for example a code on the OLED compared
   on the phone); the satellite generates its own reader identifier and
   replaces the enrolment token at once. Contacts can then come through the
   channel. *Recommendation:* keep BLE to this bootstrap of about 300 bytes.
2. **Initial contacts and the channel limits.** 3 parts per contact; a
   mailbox holds at most 32 waiting messages, and they wait until every
   full device has acknowledged them. More than about 8 contacts at setup
   can hit the limit. *Recommendation:* the outbox paces them (it already
   retries on `mailbox_full`); if that proves slow, a sealed handover slot
   with all records.
3. **When to drop the shared token without a removal.** *Recommendation:*
   once every own device has its own token on a mailbox (each device marks
   this in `device`), so that the protection holds before any removal; not
   in M10.

## Work

- Wire: types 73, 80–84, the satellite contact and satellite records.
- Relay: schema 3, token matching and roles, new calls, `/health`
  features, check order of `subscribe`, tests.
- Core and client: own tokens and enrolment, removal with revoke and
  dropping the shared token, satellite list, channel send and receive,
  removing a satellite, `lz satellite` commands.
- App: satellite in the device list, remove, notices for relays without
  `reader_tokens`.
- Satellite firmware for the ESP32-C6, on the first cut of the C library.
- Docs: `wire-format.md`, `protocol.md`, `self-hosting.md` (lost device),
  `review-brief.md`, roadmap.

## Test plan

- [ ] Relay migration from schema 2: existing mailboxes and readers work
      with the shared token; old `lz` against the new relay passes its
      tests.
- [ ] Updated devices switch to own tokens; the shared token then no
      longer subscribes as them.
- [ ] An old device receives a contact paired on a new one and reads it
      with the enrolment token.
- [ ] Removing a full device: its copy of the state gets `unknown_reader`
      and deletes itself; a saved copy can neither subscribe nor register
      itself again on any own mailbox; a new request is not answerable by
      it.
- [ ] Removing on an old relay: the client says protection is limited.
- [ ] A satellite token cannot register or revoke readers, delete the
      mailbox, drop the shared token, set the send token, derive mailboxes
      or store handover slots (all `401`).
- [ ] Satellite shows a request from the test contact; a press sends a
      `manual` response; the phone withdraws its display.
- [ ] Request answered on the phone: the satellite's light goes off.
- [ ] Rules changed on the phone reach the satellite; a request beyond the
      weekly limit is discarded by phone and satellite alike.
- [ ] A contact paired on the phone appears on the satellite; a contact
      ended disappears from it.
- [ ] Full devices drop every channel type except 80; the satellite drops
      80 and anything unknown; replays are dropped after a restart.
- [ ] Removing a full device while there is a satellite: the satellite stays
      linked and gets the new send token; the removed device's send to the
      channel is refused.
- [ ] Removing the satellite: it deletes its state on the next connection;
      the group stays; a dump of its flash cannot subscribe to any mailbox.
- [ ] Satellite clock 15 minutes off: warning on the OLED, nothing sent.
- [ ] Satellite offline, press, back online: the response arrives with the
      time of the press.
