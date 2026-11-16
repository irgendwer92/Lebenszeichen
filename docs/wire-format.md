# Lebenszeichen – Wire format

As of 2026-11-16. Makes `protocol.md` concrete for milestone M2; the sections marked M5 add several devices per person. This document is the binding specification for `crates/wire`, `crates/core` and `crates/relay` and is meant to be sufficient for writing a client in another language, for example for a microcontroller.

The decisions that go beyond `protocol.md` are collected in the section [Decisions of this document](#decisions-of-this-document) and were confirmed one by one on 2026-11-14.

Notation: All numbers are unsigned and big-endian. `b64url` means Base64 with the URL alphabet without padding (RFC 4648, section 5). `hex` means lowercase. Times are Unix seconds in UTC.

## Keys and secrets

| Value | Length | Origin |
|---|---|---|
| Contact key | X25519, 32 bytes public, 32 bytes secret | Client, new for each contact |
| Mailbox identifier | 32 bytes | Relay, random |
| Read token, send token | 32 bytes each | Client, random |
| Voucher | 32 bytes | Relay, random |
| Reader identifier | 16 bytes | Client, random, per device |
| Message identifier | 16 bytes | Client, random, per message |
| Group key | 32 bytes, symmetric | Client, random, once per person (M5) |
| Handover key | X25519, 32 bytes public, 32 bytes secret | New device, random, once per adding of a device (M5) |

Of tokens and vouchers, the relay stores only `SHA-256(value)`.

## Envelope

Every message on the wire is exactly 297 bytes long. There are two forms that the relay cannot tell apart.

**Form A, ordinary** (`crypto_box` from libsodium: X25519, XSalsa20, Poly1305):

| Offset | Length | Content |
|---|---|---|
| 0 | 1 | Protocol version, `0x01` |
| 1 | 24 | Nonce, random |
| 25 | 16 | Authentication tag |
| 41 | 256 | Encrypted plaintext |

Bytes 25 to 296 are exactly the output of `crypto_box_easy(plaintext, nonce, pk_recipient, sk_sender)`: libsodium puts the authentication tag before the ciphertext.

**Form B, sealed** (`crypto_box_seal` from libsodium), only for `hello`:

| Offset | Length | Content |
|---|---|---|
| 0 | 1 | Protocol version, `0x01` |
| 1 | 32 | Ephemeral public key |
| 33 | 16 | Authentication tag |
| 49 | 248 | Encrypted plaintext |

Bytes 1 to 296 are exactly the output of `crypto_box_seal(plaintext, pk_recipient)`.

Form B is necessary because the recipient of `hello` does not yet know the sender's key. The recipient chooses the form according to its own state: as long as a mailbox has an open invitation and no contact yet, it expects form B, afterwards form A. If a message to a contact cannot be opened in form A, the inviter tries form B: another `hello` with a key different from the contact's means that the invitation was accepted a second time, and is displayed as a warning. Anything that cannot be decrypted even then is silently discarded.

**Form C, group** (`crypto_secretbox` from libsodium: XSalsa20, Poly1305), only in the sync mailbox of a person's own devices (M5):

| Offset | Length | Content |
|---|---|---|
| 0 | 1 | Protocol version, `0x01` |
| 1 | 24 | Nonce, random |
| 25 | 16 | Authentication tag |
| 41 | 256 | Encrypted plaintext |

Bytes 25 to 296 are exactly the output of `crypto_secretbox_easy(plaintext, nonce, group_key)`. Form C has the same layout as form A, so the relay cannot tell them apart either. A device knows from the mailbox which form to expect: the sync mailbox carries only form C, contact mailboxes never.

`end` also applies before pairing is completed, in both directions. This is how a side visibly rejects a pairing whose verification code does not match.

## Plaintext

The plaintext always begins with the same header and is padded with zero bytes to the full length (256 bytes in form A, 248 bytes in form B).

| Offset | Length | Content |
|---|---|---|
| 0 | 1 | Message type |
| 1 | 16 | Message identifier |
| 17 | 8 | Time of sending |
| 25 | 2 | Length `n` of the body |
| 27 | `n` | Body, depending on the type |
| 27+`n` | rest | Zero bytes |

In form A the body may be at most 229 bytes long, in form B at most 221.

### Message types

| Type | Name | Body |
|---|---|---|
| 1 | `hello` | public contact key (32), sender's mailbox identifier (32), send token for it (32), length of the relay address (1), relay address (at most 120 bytes UTF-8) |
| 2 | `hello_ok` | empty |
| 3 | `rules` | minimum interval in minutes (4), expiry time in minutes (4), weekly limit (2) |
| 4 | `request` | empty; the message identifier is the identifier of the request |
| 5 | `response` | identifier of the request (16), kind (1): `1` `manual`, `2` `reported`; length of the reason (1), reason (at most 64 bytes UTF-8, only for kind 2, otherwise length 0) |
| 6 | `end` | empty |
| 64–70 | between own devices, only in form C | see [Messages between own devices](#messages-between-own-devices) |
| 71–127 | reserved for further messages between own devices | – |

Recipients ignore unknown types. If the body of a known type is longer than described here, the additional bytes are ignored; if it is shorter, the message is discarded.

The relay address is the base address without a trailing slash, for example `https://relay.example.org`.

### Replay protection

A recipient accepts a message only if its time is at most 24 hours and 10 minutes in the past and at most 10 minutes in the future. It remembers accepted message identifiers per contact for at least 25 hours and discards repetitions.

## Invitation

The invitation is passed on as a QR code or as text: the characters `LZ1-`, followed by Base32 (RFC 4648, uppercase, without padding) over these bytes:

| Length | Content |
|---|---|
| 1 | Version, `0x01` |
| 1 | Flags; bit 0: voucher present |
| 8 | Expiry time of the invitation |
| 32 | Mailbox identifier of the inviter |
| 32 | Send token for it |
| 32 | Public contact key of the inviter |
| 32 | Voucher, only if bit 0 is set |
| 1 | Length of the relay address |
| at most 120 | Relay address |

Base32 in uppercase fits the alphanumeric mode of QR codes and remains typable as text. When reading it in, lowercase letters, spaces and line breaks must be tolerated.

The inviter accepts exactly one `hello` per invitation, and only if it was sent before the expiry time (default: 10 minutes after creation). What counts is the sending time in the `hello`, not the time of retrieval; the inviter can retrieve it as long as the replay protection window allows, that is, until a little over 24 hours after the expiry time. Only after that does it delete the invitation and mailbox.

### Verification code

Both sides show six digits:

```
h    = SHA-256("lebenszeichen-v1-code" || smaller_key || larger_key)
code = (first 4 bytes of h as a number) mod 1000000, with leading zeros
```

"Smaller" and "larger" refer to the bytewise comparison of the two public contact keys.

## Rules for requests

The requested party checks every incoming `request` against its rules for this contact, in this order. If a condition applies, the request is silently discarded.

1. There is an open request from this contact. Open means: accepted, not yet answered manually and not yet expired.
2. Less than the minimum interval has passed since the last manual response to this contact.
3. In the last 7 × 24 hours, as many requests from this contact have already been accepted as the weekly limit allows.

Otherwise the request is accepted and counted. A reported response does not close the request; it stays open until it is answered manually or expires.

The time of a request is its sending time, but at most the recipient's own clock time at reception. Expiry time and weekly limit are calculated from this time so that both sides count the same moment, even if delivery was delayed. A request that arrives only after its expiry time is discarded and not counted.

A response is valid if it was sent while the request had not yet expired, even if it arrives later. For each request, the requester accepts at most one reported and one manual response; after a manual one, no reported one is accepted any more.

The requester applies the same check, with the other side's rules, to its own sent requests, in order to disable the button and show the next possible time. The check by the requested party remains authoritative.

Defaults: minimum interval 360 minutes, expiry time 1440 minutes, weekly limit 7. After `hello_ok`, each side sends `rules` once, and again after every change. Until the other side's rules have arrived, the defaults apply.

## Several devices (M5)

All devices of a person share the contact keys and mailboxes (`protocol.md`). Every device is a reader of every mailbox, so all of them receive every message from contacts and apply the same rules to it; the weekly limit therefore counts across all devices without further messages. What the other devices do not see are the actions of one device towards contacts: its own requests, responses, rule changes, new and removed contacts. These go through a **sync mailbox** shared by the person's devices, in form C with the **group key**.

The sync mailbox and the group key are created by the existing device when the second device is added, on the relay of its own relay account. The sync mailbox is an ordinary mailbox; every device is a reader of it with its own reader identifier and push address.

**Contact identifier:** In messages between own devices, a contact is identified by the identifier of the person's own mailbox for this contact (32 bytes). It is unique and known to every device.

### Adding a device

1. **The new device** creates a handover key and shows the device code as a QR code and as text: `LZD1-`, followed by Base32 like the invitation, over these bytes:

   | Length | Content |
   |---|---|
   | 1 | Version, `0x01` |
   | 8 | Expiry time, 10 minutes after creation |
   | 32 | Public handover key |
   | 1 | Length of the relay address |
   | at most 120 | Relay address on which the existing device stores the handover |

   Below it, the new device shows the **device check code**:

   ```
   h    = SHA-256("lebenszeichen-v1-device" || public_handover_key)
   code = (first 4 bytes of h as a number) mod 1000000, with leading zeros
   ```

2. **The existing device** reads the device code, shows the same check code and asks for confirmation that both match. It rejects expired codes and relay addresses on which it cannot store a handover. If there is no group yet, it now creates group key and sync mailbox and registers itself as a reader of it.
3. It encrypts the handover content with `crypto_box_seal` to the public handover key and stores it on the relay under the handover identifier (see [Handover](#handover)).
4. **The new device** retrieves the handover every few seconds until it arrives or the code expires, opens it and stores the state with a **new reader identifier of its own**. It registers itself as a reader with its push address on all mailboxes, including the sync mailbox, and then sends `device` to the sync mailbox.
5. The existing devices show the new device by its name. A device appearing that the person did not add is the sign of a compromised device or code.

The device code contains no secret; whoever photographs it learns nothing. Whoever slips their own device code to the person would receive the handover; the device check code protects against that, like the verification code during pairing.

**Content of the handover:** a JSON object in UTF-8:

```json
{
  "version": 1,
  "relay": { "url": "https://…", "access_token": "<hex or null>" },
  "group": { "relay": "https://…", "mailbox": "<hex>", "read_token": "<hex>",
             "send_token": "<hex>", "key": "<hex>" },
  "devices": [ { "reader": "<hex>", "name": "Phone" } ],
  "contacts": [ … ]
}
```

`contacts` contains the contacts in the same JSON form as in the state file of the existing device; open invitations stay on the device that created them and are not handed over. Reader identifier and push address are device-specific and are not part of the content.

### Messages between own devices

Same plaintext header as for contacts, form C, only in the sync mailbox:

| Type | Name | Body |
|---|---|---|
| 64 | `device` | reader identifier (16), length of the name (1), name (at most 32 bytes UTF-8) |
| 65 | `device_removed` | reader identifier (16) |
| 66 | `requested` | contact identifier (32), identifier of the request (16), time of the request (8) |
| 67 | `responded` | contact identifier (32), identifier of the request (16), kind (1) as in `response`, length of the reason (1), reason (at most 64 bytes UTF-8), time of the response (8) |
| 68 | `rules_set` | contact identifier (32), the person's own rules for this contact as in `rules` (10) |
| 69 | `contact_part` | contact identifier (32), number of this part (1, from 0), number of parts (1), length (1), data (at most 194 bytes) |
| 70 | `contact_removed` | contact identifier (32) |

- `requested`: another device of the person sent a request to this contact. The recipient shows it as open and accepts the contact's response to it.
- `responded`: another device answered the request. The recipient withdraws its display and lets the minimum interval start from the time of the response.
- `contact_part`: a contact paired on another device. The parts, put together in order, are the contact's entry in the JSON form of the handover. The recipient keeps parts for at most 24 hours, adds the contact when all parts are there and registers itself as a reader of its mailbox.
- If two devices change the same thing, the later sending time wins.
- A device enters the identifiers of its own messages into its replay protection before sending. It also reads the sync mailbox and thus discards its own messages as duplicates.
- Replay protection, time window and handling of unknown types are the same as for contacts.

### Removing a device

Another device of the person removes the reader identifier of the device from all mailboxes, including the sync mailbox, and sends `device_removed`. If the removed device is still running, the relay answers it with `unknown_reader`; it then shows that it was removed and deletes its state.

Removing a device is not protection against a lost or stolen device: it keeps all contact keys and the group key. In that case, the contacts have to be paired again (`protocol.md`).

### Known gaps

- A device that is offline for more than 24 hours misses messages, because the relay deletes them after 24 hours. Its counters and display can deviate until the next message about the same contact.
- Between the handover and the new device's registration as a reader, the other devices may acknowledge messages that the new device then never receives.

## Relay interface

HTTPS and WebSocket under the path `/v1`. Tokens are in the header `Authorization: Bearer <b64url>`. Identifiers in paths and JSON are `b64url`. Errors have a JSON body `{"error":"<code>"}`.

| Call | Authorization | Request | Response |
|---|---|---|---|
| `GET /v1/health` | – | – | `200` |
| `POST /v1/vouchers` | Access token | – | `201 {"voucher":"…","expires_in":600}` |
| `POST /v1/mailboxes` | Access token, voucher, or read token of a voucher mailbox (M5) | `{"read_token_hash":"<hex>","send_token_hash":"<hex>"}` | `201 {"mailbox":"…"}` |
| `DELETE /v1/mailboxes/{mailbox}` | Read token | – | `204` |
| `PUT /v1/mailboxes/{mailbox}/readers/{reader}` | Read token | `{"push_url":"https://…"}` or `{"push_url":null}` | `204` |
| `DELETE /v1/mailboxes/{mailbox}/readers/{reader}` | Read token | – | `204` |
| `POST /v1/mailboxes/{mailbox}/messages` | Send token | Body: the 297 bytes of the envelope, `application/octet-stream` | `202` |
| `PUT /v1/handovers/{handover}` | Access token or read token of any mailbox on this relay (M5) | Body: the sealed handover, at most 64 KiB | `201` |
| `GET /v1/handovers/{handover}` | – | – | `200` with the sealed handover as body; deleted afterwards |

Error codes:

| Status | `error` | Meaning |
|---|---|---|
| 400 | `bad_request` | Request not readable |
| 401 | `unauthorized` | Token missing or does not match |
| 404 | `unknown_mailbox` | The mailbox does not exist or no longer exists. This is how a client recognizes that it was removed. |
| 404 | `unknown_reader` | Reader not registered |
| 404 | `unknown_handover` | No handover under this identifier, or already retrieved or expired |
| 409 | `handover_exists` | A handover under this identifier exists already |
| 429 | `mailbox_quota` | The voucher mailbox already has 10 derived mailboxes |
| 503 | `handovers_full` | 64 handovers are waiting on this relay |
| 413 | `too_large` | Body longer than 512 bytes, for a handover longer than 64 KiB |
| 429 | `mailbox_full` | Already 8 waiting messages |
| 429 | `rate_limited` | More than 20 deliveries in the last hour |

A voucher is valid for 10 minutes and for exactly one mailbox. The relay accepts arbitrary bodies up to 512 bytes and does not check their content.

**Derived mailboxes (M5):** Whoever shows the read token of a mailbox that was created with a voucher may create up to 10 further mailboxes with it. They count as long as they exist; deleting one frees its place. Derived mailboxes cannot create further mailboxes, and neither can mailboxes created with the access token. This lets people without an access token create a sync mailbox for several devices, and later also invite contacts themselves, without letting strangers onto the relay: every derived mailbox goes back to a voucher issued by a holder of the access token.

### Handover

A handover takes the state of a person to a new device (see [Adding a device](#adding-a-device)).

```
handover = b64url(SHA-256("lebenszeichen-v1-handover" || public_handover_key))
```

- Only the new device knows the public handover key before the device code is shown, so the identifier cannot be guessed in advance. The content is sealed for the new device; the relay cannot read it.
- `PUT` needs a token of this relay, to keep strangers from storing data on it. The read token of any mailbox suffices, so that people without an access token can add devices as well.
- `GET` needs no token: the identifier is the authorization. The relay deletes the handover with the first successful `GET`, at the latest 10 minutes after the `PUT`.
- At most 64 handovers wait on a relay at the same time.

### Delivery

`GET /v1/stream` opens a WebSocket. All frames are text with a JSON object.

From the client:

```json
{"op":"subscribe","mailbox":"…","token":"<read token>","reader":"…"}
{"op":"ack","mailbox":"…","seq":17}
```

From the relay:

```json
{"op":"subscribed","mailbox":"…"}
{"op":"message","mailbox":"…","seq":17,"data":"<envelope as b64url>"}
{"op":"error","mailbox":"…","code":"unknown_mailbox"}
```

- A connection can subscribe to any number of mailboxes. The reader must be registered beforehand.
- `seq` counts upward per mailbox and starts at 1. After `subscribed`, the relay sends all waiting messages with `seq` above this reader's last acknowledgment, then new ones as they arrive.
- `ack` acknowledges all messages of this mailbox up to and including `seq`. A client acknowledges only once it has durably stored or processed the message.
- The relay deletes a message as soon as all registered readers have acknowledged it, at the latest 24 hours after arrival. If a mailbox has no reader, the message stays for up to those 24 hours.
- Mailboxes without retrieval for 90 days are deleted.

### Wake signal

When the relay accepts a message, it sends a `POST` with the fixed body `wake` to the `push_url` of each reader that is not currently subscribed to this mailbox. The body carries no information; an empty body is not delivered by some distributors. Failures are not retried and not reported.

Because a client can use this to make the relay call foreign addresses, the following applies: only `https`, no private or local target addresses, no redirects, short timeout. The operator can restrict the allowed host names to a list and should do so, for example to their own ntfy server.

### Operation

Settings via environment variables:

| Variable | Meaning | Default |
|---|---|---|
| `LZ_ACCESS_TOKEN` | Access token as `b64url` of 32 bytes; required | – |
| `LZ_LISTEN` | Address and port | `0.0.0.0:8080` |
| `LZ_DB` | Path of the SQLite file | `/data/relay.sqlite` |
| `LZ_PUSH_ALLOWED_HOSTS` | Allowed host names for wake signals, separated by commas; empty means all public ones | empty |

The relay writes no IP addresses and no mailbox identifiers to its output. TLS is handled by the reverse proxy in front of it.

## Decisions of this document

These points go beyond `protocol.md`. Gone through and confirmed one by one on 2026-11-14:

- **Fixed binary format** instead of JSON or CBOR in the plaintext: small, no parser needed and simple on microcontrollers.
- **Sealed form for `hello`**, because the recipient does not yet know the sender.
- **Authentication tag before the ciphertext**, as libsodium outputs it. The first version of this document had it after; corrected on 2026-11-14, before code was built on it.
- **Invitation as Base32 with prefix `LZ1-`**; this answers the open question on the format from `protocol.md`.
- **Verification code via SHA-256.**
- **The minimum interval counts from the manual response**; a reported response does not close the request.
- **Acceptance of an invitation is judged by the sending time of the `hello`**, so that the inviter does not have to retrieve within 10 minutes. Price: the sender sets the sending time itself. Whoever intercepts a code can send a backdated `hello`; an intercepted code thus remains usable for a little over 24 hours instead of 10 minutes. It is still valid only once, and the verification code shows that the wrong contact was accepted.
- **The time of a request is its sending time**, limited by the recipient's clock; late-arriving requests and responses sent in time are judged by it.
- **Replay protection window** of 24 hours plus 10 minutes tolerance. A device with a badly wrong clock therefore cannot participate.
- **Relay: JSON for control, raw bytes for messages, cumulative acknowledgments.**
- **Wake signal only to readers without an open connection**, with the fixed body `wake`.
- **Protection of the relay against abusive push addresses.**
- **SQLite as the relay's storage.** The core talks to its storage through an interface and stores in one JSON file per device (see `concept.md`).

### Decisions for M5

Decided on 2026-11-16 (E1 to E6 and the relay rule), going beyond `protocol.md`:

- **The new device shows the code**, with a public key only; the existing device seals the state to it. A device check code protects against a swapped code.
- **Handover through the relay** as a separate, one-time block of up to 64 KiB, because the state does not fit into a few messages of 256 bytes and the limits per mailbox (8 waiting, 20 per hour) would make the transfer take hours.
- **No `counted` message:** all devices read all mailboxes and count the same requests. Instead `requested`, so that other devices accept the response to a request of the person's own.
- **Devices offline for more than 24 hours** may deviate; accepted for M5.
- **Removing a device is not protection** against a lost device; the contacts are paired again.
- **Invitations stay on the device** that created them.
- **Derived mailboxes:** up to 10 per voucher mailbox, not chainable, so that several devices also work without an access token.
- **Content of the handover as JSON** in the form of the state file, instead of a fixed binary format. Simple and extensible; the fixed binary format of the messages is meant for microcontrollers, which are not expected to add other devices.
- **`responded` instead of `done`**, with kind and time of the response, so that other devices start the minimum interval at the right time.

Clarifications made during the implementation of the relay (`crates/relay`). They are implementation details and were not confirmed one by one:

- **Order of checks:** If the token is missing or is not `b64url` of 32 bytes, the result is `401`. Then the mailbox is looked up; an unknown mailbox reports `404 unknown_mailbox` even if the token would not match, so that a removed client recognizes this reliably. Only then is the token compared (`401`).
- **Unreadable identifiers** in paths (not `b64url` or wrong length) result in `400 bad_request`. Hashes in `POST /v1/mailboxes` must be 64 hex digits; uppercase letters are tolerated.
- **Voucher:** It is passed like a token in the header `Authorization: Bearer …`. An unknown, redeemed or expired voucher results in `401`. It is valid for exactly 600 seconds.
- **Empty body** in `POST …/messages` results in `400 bad_request`. The `Content-Type` is not checked.
- **Limits exceeded at the same time:** The rate limit is checked before the fill level, and `rate_limited` is returned. Rejected deliveries do not count. "Waiting" means: still stored, even if individual readers have already acknowledged it.
- **Rate limit only in memory:** The relay keeps the arrival times for the limit of 20 per hour only in memory and only for one hour. After a restart, counting starts anew.
- **Errors of the relay itself** report `500` with `{"error":"internal"}`.
- **Readers:** At most 16 readers per mailbox, above that `400 bad_request`, because every reader can trigger a wake signal. A newly registered reader starts with acknowledgment 0 and receives all waiting messages; a repeated `PUT` only changes the push address. A body `{}` counts as `{"push_url":null}`. When a reader is unregistered, the relay immediately deletes the messages that all remaining readers have acknowledged.
- **Push address:** at most 1024 bytes, only `https`, without user name and password, not `localhost`, and as an IP address only public ones. The relay resolves the host name itself when waking and connects only to the checked public addresses (IPv4 and IPv6), so that a second resolution cannot return anything different. No proxy, no redirects, timeout 5 seconds. `LZ_PUSH_ALLOWED_HOSTS` compares the host name exactly, case-insensitively and without subdomains. Invalid addresses result in `400 bad_request` on registration.
- **Day of last use:** It is set by creating the mailbox, registering a reader, `subscribe` and `ack`, but not by delivery. A mailbox whose day is more than 90 days in the past is deleted. The relay cleans up every 5 minutes.
- **Acknowledgments:** `seq` is never reused, not even after messages are deleted. An acknowledgment beyond the highest assigned `seq` is capped to it so that it does not cover future messages. The relay does not respond to `ack`, except with an error.
- **WebSocket errors:** `code` is one of the error codes above (`bad_request`, `unauthorized`, `unknown_mailbox`, `unknown_reader`, `internal`). For an unreadable frame, the field `mailbox` is missing. `ack` for a mailbox that this connection has not subscribed to results in `bad_request`. If a subscribed mailbox is deleted or the reader is unregistered, the relay sends `unknown_mailbox` or `unknown_reader` respectively and ends the subscription. A repeated `subscribe` on the same connection to the same mailbox replaces the old subscription. At most 256 mailboxes per connection, frames at most 4 KiB.

Not specified: the messages between own devices (M5) and the behavior when changing the relay.
