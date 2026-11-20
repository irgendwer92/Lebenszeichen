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
| Fetch token, secret value `n` | 32 bytes each | New device, random, once per adding of a device (M5) |
| Challenge, reply token | 32 bytes each | Existing device, random, once per adding of a device (M5) |

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

In form A the body may be at most 228 bytes long, in form B at most 221.

**Direction marker (form A, since M9).** The last byte of the plaintext of
form A (offset 255) is `1` if the sender's public contact key is the lower
of the two (compared bytewise), `2` if it is the higher one. A recipient
discards a message that carries its own marker. `crypto_box` gives both
directions the same key; without the marker a relay could hand a message
back to its sender, where it would open as if the contact had sent it (a
request shown as the contact's, an own answer shown as theirs). Older
clients write `0` there (padding); `0` is accepted until all clients send the
marker, so the protection holds for messages of updated senders. Found in
the security review of M9 (2026-11-19).

### Message types

| Type | Name | Body |
|---|---|---|
| 1 | `hello` | public contact key (32), sender's mailbox identifier (32), send token for it (32), length of the relay address (1), relay address (at most 120 bytes UTF-8) |
| 2 | `hello_ok` | empty |
| 3 | `rules` | minimum interval in minutes (4), expiry time in minutes (4), weekly limit (2) |
| 4 | `request` | empty; the message identifier is the identifier of the request |
| 5 | `response` | identifier of the request (16), kind (1): `1` `manual`, `2` `reported`; length of the reason (1), reason (at most 64 bytes UTF-8, only for kind 2, otherwise length 0) |
| 6 | `end` | empty |
| 7 | `reveal` | the inviter's secret value `n` (32) |
| 64–70 | between own devices, only in form C | see [Messages between own devices](#messages-between-own-devices) |
| 71–127 | reserved for further messages between own devices | – |

Recipients ignore unknown types. If the body of a known type is longer than described here, the additional bytes are ignored; if it is shorter, the message is discarded.

The relay address is the base address without a trailing slash, for example `https://relay.example.org`.

### Replay protection

A recipient accepts a message only if its time is at most 24 hours and 10 minutes in the past and at most 10 minutes in the future. It remembers accepted message identifiers per contact for at least 25 hours and discards repetitions.

## Invitation

The invitation is passed on as a QR code or as text: the characters `LZ2-`, followed by Base32 (RFC 4648, uppercase, without padding) over these bytes:

| Length | Content |
|---|---|
| 1 | Version, `0x02` |
| 1 | Flags; bit 0: voucher present |
| 8 | Expiry time of the invitation |
| 32 | Mailbox identifier of the inviter |
| 32 | Send token for it |
| 32 | Public contact key of the inviter |
| 32 | Commitment `SHA-256("lebenszeichen-v2-invite-commit" \|\| n)` to the inviter's secret value `n` |
| 32 | Voucher, only if bit 0 is set |
| 1 | Length of the relay address |
| at most 120 | Relay address |

Base32 in uppercase fits the alphanumeric mode of QR codes and remains typable as text. When reading it in, lowercase letters, spaces and line breaks must be tolerated.

`n` is 32 random bytes that the inviter creates with the invitation and keeps secret until it has received the `hello`. Invitations of version 1 (`LZ1-`, without commitment) are refused with a hint to ask for a new invitation (decided on 2026-11-16); contacts paired with them stay as they are. An invitation with prefix `LZ2-` but another version byte is refused as well. After the update, open invitations of version 1 are deleted together with their mailbox. All prefixes in this document are ASCII bytes without a terminator.

The inviter accepts exactly one `hello` per invitation, and only if it was sent before the expiry time (default: 10 minutes after creation). What counts is the sending time in the `hello`, not the time of retrieval; the inviter can retrieve it up to one hour after the expiry time, then it deletes the invitation and mailbox. (Until M9 the limit was the replay protection window, a little over 24 hours; the security review of M9 noted that the sending time is the sender's word, so someone who got hold of an unused code could backdate a `hello` for that long.)

### Verification code

The verification code is derived from both public contact keys and the inviter's secret value `n`, which reaches the accepting side only after its `hello` is fixed:

1. The accepting side sends `hello` as before.
2. The inviter, on receiving the `hello`, sends `reveal` with `n` right away, without waiting for the person; in form A, because it now knows the contact key of the accepting side.
3. The accepting side checks `n` against the commitment from the invitation. If it does not match, it shows an alarm, sends `end` and deletes its mailbox.
4. Both sides show six digits:

```
h    = SHA-256("lebenszeichen-v2-code" || key_of_inviter || key_of_accepter || n)
code = (first 4 bytes of h as a number) mod 1000000, with leading zeros
```

Whoever swaps an invitation in transit and poses as the accepting side towards the inviter must fix its key before the inviter reveals `n`; towards the accepting side, it is bound by the commitment in its own invitation, and the accepting side's key is new. Neither side leaves anything to search for, so matching digits are a coincidence of one in a million. The first version derived the code from the two keys alone; someone who controls the channel of an invitation could match it by searching about a million keys (crypto review of 2026-11-16).

**Accepting side:**

- It generates its contact key after reading the invitation, new for each accepted invitation, also when retrying, and never shows or sends it before the `hello`. This fresh key is its unpredictable contribution; it adds no value of its own.
- It stores its pending state (secret key, commitment, own mailbox) before sending `hello`.
- Until a valid `reveal` has arrived, it shows no code and is not paired; a `hello_ok` received before that is kept and applied only after a valid `reveal`. It accepts only the first `reveal` that matches the commitment; any `reveal` that does not match triggers step 3. There is no code of version 1 on either side.
- If no valid `reveal` arrives within 24 hours and 10 minutes after the `hello` was sent, it shows "Kopplung fehlgeschlagen", sends `end` and deletes its mailbox.

**Inviter:**

- It sends `reveal` only for a `hello` that is accepted, that is, sent before the expiry time of the invitation. A `hello` that fails this check gets nothing and does not use up the invitation.
- It records the accepted key and stores the contact before sending `reveal`, in one step, so that two `hello`s fetched together never both get a `reveal`.
- A `hello` repeated with the same key as the first gets the same `reveal` again, which recovers from a lost `reveal`. A `hello` with a different key gets none and is shown as a warning, as before.
- It keeps `n` with the contact until the person confirms or rejects the pairing, so that the code survives a restart, and deletes it then. Only the inviting device holds `n`; other devices of the person learn about the contact only after the pairing is complete.

**People:** The digits must be compared over another channel than the one the invitation took: in person, on the phone, or through another messenger. If invitation and comparison both go through a channel that someone controls, that someone can pass the digits on as well. Interfaces say so next to the code.

### Setup code

A holder of the access token can show a setup code to set up another person's device on the same relay without typing: `LZS1-`, followed by Base32 like the invitation, over these bytes:

| Length | Content |
|---|---|
| 1 | Version, `0x01` |
| 32 | Access token |
| 1 | Length of the relay address |
| at most 120 | Relay address |

The setup code carries the access token itself and therefore does not expire: whoever sees it can create mailboxes on the relay until the operator changes the token. It is only shown directly, never sent. Interfaces recognize the kind of a code by its prefix: `LZ2-` invitation (`LZ1-` is recognized only to refuse it), `LZD1-` device code, `LZS1-` setup code.

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

The new device **N** and an existing device **E** of the person exchange four things through handover slots on the relay (see [Handover](#handover)): a challenge from E, a revealed value from N, then the handover. Two six-digit codes are compared on the way; the person confirms the first on E and the second on N.

1. **N** creates a handover key, a fetch token, a secret value `n` (32 bytes each, random) and a reader identifier of its own. It shows the **device code** as a QR code and as text: `LZD1-`, followed by Base32 like the invitation, over these bytes:

   | Length | Content |
   |---|---|
   | 1 | Version, `0x01` |
   | 8 | Expiry time, 10 minutes after creation |
   | 32 | Public handover key |
   | 16 | Reader identifier of N |
   | 32 | `SHA-256(fetch token)` |
   | 32 | Commitment `SHA-256("lebenszeichen-v1-commit" \|\| n)` |
   | 1 | Length of the relay address |
   | at most 120 | Relay address on which E stores the slots |

2. **E** reads the device code. It refuses expired codes and relay addresses on which it has neither its relay account nor a mailbox of its own; it only ever sends tokens to the relay that issued them. It creates a random **challenge** `c` and a random **reply token** `t` (32 bytes each) and stores `c || t` in the challenge slot

   ```
   challenge_slot = SHA-256("lebenszeichen-v1-challenge" || public_handover_key)
   ```

   with the hash of the fetch token from the code and `SHA-256(t)` as reply hash. If the relay answers `409 handover_exists`, E shows an alarm and stops.

3. **N** fetches the challenge slot with its fetch token. Only then it reveals `n`: it stores `n` in the reveal slot that belongs to the challenge slot, authorized with `t`. N reveals `n` at most once per device code and never accepts a second challenge for it; on any failure it starts over at step 1 with a new handover key, fetch token and `n`.

4. **E** fetches the reveal slot with `t` and checks `n` against the commitment from the device code. If it does not match, E shows an alarm and stops. **Both** now show the **device check code**:

   ```
   h    = SHA-256("lebenszeichen-v1-device" || public_handover_key || n || c)
   code = (first 4 bytes of h as a number) mod 1000000, with leading zeros
   ```

   The person compares both and confirms on E. Whoever swaps the device code in transit has to fix its own `n` (through the commitment) before it sees `c`, and has to hand N a challenge before N reveals `n`. Neither side leaves anything to search for, so the check also protects a device code that was passed on as text.

5. **E** prepares the state. If there is no group yet, it creates group key and sync mailbox and registers itself as a reader of it. It registers the reader identifier of N, without push address, on all mailboxes of the person including the sync mailbox, but without the mailboxes of open invitations, **before** it takes the snapshot, so that no message reaches only the old devices from then on. Then it seals the handover content with `crypto_box_seal` to the public handover key and stores it in the handover slot

   ```
   handover_slot = SHA-256("lebenszeichen-v1-handover" || public_handover_key)
   ```

   with the hash of the fetch token. On `409 handover_exists` E shows an alarm, removes the reader identifier of N again and stops.

6. **N** fetches the handover slot with its fetch token and opens the handover. If its `challenge` is not the `c` from step 3, N shows an alarm and discards it. Otherwise both devices show the **handover check code**:

   ```
   h    = SHA-256("lebenszeichen-v1-handover-check" || group_key || public_handover_key)
   code = (first 4 bytes of h as a number) mod 1000000, with leading zeros
   ```

   The person confirms on N. Only someone who knows the group key can produce matching digits, so a handover planted by a third party, including the relay operator, is recognized here. After confirming, N stores the state, registers its push address on all mailboxes and sends `device` to the sync mailbox, all before the expiry time of the device code; otherwise the adding has failed and N starts over.

7. If E receives no `device` with the reader identifier of N until the expiry time, it removes that reader identifier from all mailboxes again. The other devices show the new device by its name; a device that the person did not add is the sign of a compromised device or code.

**Content of the handover:** a JSON object in UTF-8:

```json
{
  "version": 1,
  "challenge": "<hex>",
  "relay": { "url": "https://…", "access_token": "<hex or null>" },
  "group": { "relay": "https://…", "mailbox": "<hex>", "read_token": "<hex>",
             "send_token": "<hex>", "key": "<hex>", "seen": [ … ] },
  "devices": [ { "reader": "<hex>", "name": "Phone" } ],
  "contacts": [ … ]
}
```

- `group.seen` is the replay protection of the sync mailbox (message identifiers with their times), so that N discards sync messages that are already reflected in the snapshot.
- `contacts` contains the contacts in the same JSON form as in the state file of E, including their replay protection and counters. Open invitations stay on the device that created them and are not handed over. Reader identifier and push address of E are device-specific and are not part of the content.
- Unlike the messages, the handover content is not specified byte by byte here: it is bound to the state file of Lebenszeichen's implementation (`crates/core`, `store.rs`) and carries its own `version`. A device refuses a handover with a version it does not know; the person then updates the older device first.

### Contact record

`contact_part` carries a newly paired contact as a compact record, only with what cannot be derived:

| Length | Content |
|---|---|
| 32 | Secret contact key |
| 32 | Public contact key of the contact |
| 32 | Read token of the own mailbox |
| 1 + at most 120 | Own relay address (length, address) |
| 32 | Mailbox identifier of the contact |
| 32 | Send token for it |
| 1 + at most 120 | Relay address of the contact (length, address) |
| 10 | Own rules for this contact, as in `rules` |
| 8 + 16 | Sending time and message identifier of the last applied own rules |
| 10 | Rules of the contact, as in `rules` |
| 8 + 16 | Sending time and message identifier of the last applied rules of the contact |
| 1 + at most 64 | Local name of the contact (length, name in UTF-8) |

The own mailbox identifier is the contact identifier of the message. Replay protection and counters of the new contact start empty on the receiving device.

### Messages between own devices

Same plaintext header as for contacts, form C, only in the sync mailbox:

| Type | Name | Body |
|---|---|---|
| 64 | `device` | reader identifier (16), length of the name (1), name (at most 32 bytes UTF-8) |
| 65 | `device_removed` | reader identifier (16) |
| 66 | `requested` | contact identifier (32), identifier of the request (16), time of the request (8) |
| 67 | `responded` | contact identifier (32), identifier of the request (16), kind (1) as in `response`, length of the reason (1), reason (at most 64 bytes UTF-8), time of the response (8) |
| 68 | `rules_set` | contact identifier (32), the person's own rules for this contact as in `rules` (10) |
| 69 | `contact_part` | contact identifier (32), transfer identifier (16, random per transfer), number of this part (1, from 0), number of parts (1), length (1), data (at most 178 bytes) |
| 70 | `contact_removed` | contact identifier (32) |
| 71 | `state_shared` | length of the reason (1), reason (at most 64 bytes UTF-8), end of the state (8, Unix seconds) |
| 72 | `state_cleared` | – |

- `requested`: another device of the person sent a request to this contact. The recipient shows it as open and accepts the contact's response to it.
- `responded`: another device answered the request. The recipient withdraws its display and lets the minimum interval start from the time of the response.
- `rules_set`: the recipient takes the rules only if they are newer than the last ones it applied for this contact. Newer means: later sending time, or the same sending time and a larger message identifier (bytewise). The same comparison applies to `rules` from a contact, because two devices of the contact may have sent them.
- `contact_part` is sent by the device that completed the pairing, as soon as it has sent or received `hello_ok`. The parts of one transfer, put together in order of their numbers, are the [contact record](#contact-record). The recipient discards the whole transfer if the number of parts differs between parts, a number is not below the number of parts, a length is larger than 178 or than the rest of the body, the record cannot be read completely, or the transfer is not complete within 24 hours. If the contact already exists, the transfer is ignored. Otherwise the recipient adds the contact and registers itself as a reader of the own mailbox with its push address.
- `contact_removed`: another device ended the contact; it has already sent `end` and deleted the mailbox. The recipient only deletes the contact locally.
- `state_shared` (M8c): another device hands on its reported state. The recipient keeps it as its handed state and answers its contacts with it where reported answers are allowed on the recipient, after its own waiting time; of the own and the handed state the valid one set last applies. It also drops an own state that was shared, as all shared states of a person form one. `state_cleared` ends the handed state and an own shared state; an own state that was never shared stays. Both are applied only if newer than the last `state_shared` or `state_cleared` sent or applied by this device, compared as for `rules_set`; a newer one whose end has passed still counts as the newest. See `m8c-share-state.md`.
- A device enters the identifiers of its own messages into the replay protection of the sync mailbox before sending. It also reads the sync mailbox and thus discards its own messages as duplicates.
- Replay protection, time window and handling of unknown types are the same as for contacts, with one replay protection for the whole sync mailbox.
- A sync message about a contact that the recipient does not know yet is kept for up to 24 hours and applied as soon as the contact arrives through `contact_part`. Otherwise a new device would show a request as open that another device has already answered.
- If the relay refuses a message to the sync mailbox with `mailbox_full` or `rate_limited`, the device keeps it and tries again with every later fetch, at the latest every 5 minutes; after 24 hours it drops it.

### Removing a device

Another device of the person (E) removes the reader identifier of the device from all mailboxes, then **ends the group**: it deletes the sync mailbox and forgets the group key and the list of devices (since M9).

- The removed device knows the group key and the tokens of the sync mailbox. With the group kept, it could register itself there again, receive the contacts paired from then on (`contact_part`) and send forged sync messages, such as `device_removed` for E. Without a channel it cannot read, a new group key cannot reach the other devices, so they are added again instead (security review of M9, 2026-11-19).
- A device whose sync mailbox reports `unknown_mailbox` waits until all its subscriptions have answered. If one of its contact mailboxes reports `unknown_reader`, it was removed: it shows that and deletes its state. Otherwise another device was removed: it leaves the group, keeps its contacts and works on alone, with a notice that it has to be added again to be linked.
- `device_removed` is still understood (older devices send it), but no longer sent.

Removing still does not protect the existing contacts: the removed device keeps their keys and tokens and could register itself again on their mailboxes. After a lost or stolen device the contacts are paired again (`protocol.md`); the new pairings are out of its reach.

### Known gaps

- A device that is offline for more than 24 hours misses messages, because the relay deletes them after 24 hours. Its counters and display can deviate until the next message about the same contact. A missed `contact_part` is not repeated: the contact stays missing on that device until it is added again. A way to fetch the complete state again is left for later.

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
| `PUT /v1/handovers/{handover}` | Access token or read token of any mailbox on this relay (limits per voucher family since M9); for a reveal slot the reply token (M5) | Header `Lz-Fetch-Hash: <hex>`, optionally `Lz-Reply-Hash: <hex>`; body: at most 64 KiB, `application/octet-stream` | `201` |
| `GET /v1/handovers/{handover}` | Fetch token; for a reveal slot the reply token (M5) | – | `200` with the stored bytes as body; deleted afterwards |

Error codes:

| Status | `error` | Meaning |
|---|---|---|
| 400 | `bad_request` | Request not readable; for a message, a body that is not exactly 297 bytes |
| 401 | `unauthorized` | Token missing or does not match |
| 404 | `unknown_mailbox` | The mailbox does not exist or no longer exists. This is how a client recognizes that it was removed. |
| 404 | `unknown_reader` | Reader not registered |
| 404 | `unknown_handover` | No handover under this identifier, or already retrieved or expired |
| 409 | `handover_exists` | A handover under this identifier exists already |
| 429 | `mailbox_quota` | The voucher mailbox already has 10 derived mailboxes, or has derived 30 in its lifetime |
| 429 | `handovers_quota` | Two handovers authorized with this token, or with read tokens of the same voucher family, are waiting already |
| 503 | `handovers_full` | 64 handovers are waiting on this relay, or 32 authorized by voucher families |
| 413 | `too_large` | Handover longer than 64 KiB |
| 429 | `mailbox_full` | Already 32 waiting messages |
| 429 | `rate_limited` | More than 60 deliveries in the last hour |

A voucher is valid for 10 minutes and for exactly one mailbox. The relay accepts only message bodies of exactly 297 bytes, the length of an envelope, and does not check their content (since M9; before, any body up to 512 bytes).

**Derived mailboxes (M5):** Whoever shows the read token of a mailbox that was created with a voucher may create up to 10 further mailboxes with it. They count as long as they exist; deleting one frees its place, but a voucher mailbox can derive at most 30 mailboxes in its lifetime, so that deleting and creating again cannot go on forever (since M9). Derived mailboxes cannot create further mailboxes, and neither can mailboxes created with the access token. The relay checks the count and creates the mailbox in one step, so that simultaneous requests cannot exceed 10. A derived mailbox stays when its voucher mailbox is deleted and then follows only the 90-day rule; it can no longer be used to create further ones, because the voucher mailbox is gone. This lets people without an access token create a sync mailbox for several devices, and later also invite contacts themselves, without letting strangers onto the relay: every derived mailbox goes back to a voucher issued by a holder of the access token. The holder of the access token cannot withdraw such mailboxes; that is the price of the relay not knowing who belongs to whom.

### Handover

Handover slots carry challenge, revealed value and handover when a device is added (see [Adding a device](#adding-a-device)). The identifier `{handover}` is the `b64url` of the SHA-256 values given there.

- `PUT` needs a token of this relay, to keep strangers from storing data on it. The read token of any mailbox suffices, so that people without an access token can add devices as well. At most two slots authorized with the same token wait at the same time, at most 64 on the relay.
- **Voucher families (M9):** A voucher mailbox and the mailboxes derived from it form a voucher family; a derived mailbox stays in it when the voucher mailbox is deleted. Slots authorized with read tokens of one voucher family share the limit of two, and all voucher families together hold at most 32 slots, reserved reveal slots included. The access token and the read tokens of mailboxes created with it are not limited this way, so guests cannot keep the holder of the access token from adding devices. If several mailboxes have the same read token hash and one of them belongs to a voucher family, that family's limits apply.
- `PUT` names the hash of the fetch token in `Lz-Fetch-Hash`; `GET` needs the fetch token itself. Whoever only sees the device code can therefore neither read nor delete a slot.
- **Reveal slot:** If a `PUT` also names `Lz-Reply-Hash`, the relay reserves the slot `SHA-256("lebenszeichen-v1-reveal" || slot)` in the same step, where `slot` are the 32 bytes of the identifier just stored. If that reveal identifier exists or is reserved already, the whole `PUT` fails with `409`. The reservation lasts 10 minutes from the reserving `PUT`, independent of when the reserving slot is fetched or deleted. The reveal slot accepts exactly one `PUT` of exactly 32 bytes, authorized with the reply token in `Authorization: Bearer` instead of a relay token and without `Lz-Fetch-Hash`; `GET` on it also needs the reply token, and a successful `GET` deletes it. A reserved or filled reveal slot counts towards the limit of 64 slots on the relay and, if reserved by a voucher family, towards the 32 of all voucher families, not towards the two per token or per family. This lets the new device answer without a token of the relay.
- The relay deletes a slot with the first successful `GET`, at the latest 10 minutes after the `PUT`. A second `PUT` to an existing or reserved identifier fails with `409`.
- The relay does not look into the content: challenge and revealed value are not secret, only unpredictable in advance, and the handover is sealed for the new device.
- Whoever holds read tokens on the relay can occupy slots up to the limits and thus delay the adding of devices by others; this is a possible disruption, not a leak.

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

Because a client can use this to make the relay call foreign addresses, the following applies: only `https` on port 443, no private or local target addresses, no redirects, short timeout. The operator can restrict the allowed host names to a list and should do so, for example to their own ntfy server.

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
- **Invitation as Base32 with prefix `LZ1-`**; this answers the open question on the format from `protocol.md`. Since 2026-11-16 `LZ2-`, see the decisions for the pairing fix.
- **Verification code via SHA-256.** Replaced on 2026-11-16 by the version with commitment and `reveal` (see the decisions for the pairing fix).
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

- **The new device shows the code**, with a public key only; the existing device seals the state to it.
- **Device check code over a commitment of the new device and a challenge of the existing device**, revealed in this order, so that a swapped code cannot be matched by searching; and a **handover check code** from the group key against a planted handover. Both after two crypto reviews on 2026-11-16; the first fix, a challenge alone, could still be matched by an attacker who chooses the challenge the new device sees.
- **Handover through the relay** as a separate, one-time block of up to 64 KiB, because the state does not fit into a few messages of 256 bytes and the limits per mailbox (32 waiting, 20 per hour) would make the transfer take hours. Fetching needs a fetch token that only the new device knows.
- **At most 32 waiting messages per mailbox** instead of 8, for all mailboxes: a sync mailbox must not fill up just because one device is offline for a day. Costs about 10 KB per mailbox; the limit of 20 per hour stays.
- **New contacts as a compact binary record** (at most 535 bytes) in 2 to 4 parts with a transfer identifier, instead of their JSON form, which would need about 5 parts.
- **Last writer wins** for rules, by sending time and then message identifier; this also applies to `rules` from contacts with several devices.
- **No `counted` message:** all devices read all mailboxes and count the same requests. Instead `requested`, so that other devices accept the response to a request of the person's own.
- **Devices offline for more than 24 hours** may deviate; accepted for M5.
- **Removing a device is not protection** against a lost device; the contacts are paired again. Since M9 removing ends the group, so that contacts paired again do not reach the removed device.
- **Invitations stay on the device** that created them.
- **Derived mailboxes:** up to 10 per voucher mailbox, not chainable, so that several devices also work without an access token.
- **Content of the handover as JSON** in the form of the state file, instead of a fixed binary format. Simple and extensible; the fixed binary format of the messages is meant for microcontrollers, which are not expected to add other devices.
- **`responded` instead of `done`**, with kind and time of the response, so that other devices start the minimum interval at the right time.

### Decisions for the pairing fix

Decided on 2026-11-16, after the crypto review of M5 found that the verification code of version 1 can be matched by someone who controls the channel of an invitation:

- **Commitment in the invitation, `reveal` after the `hello`.** The inviter commits to a secret value in the invitation and reveals it only after the `hello` has arrived; the code covers both keys and that value. `hello` stays unchanged: the accepting side's fresh key serves as its unpredictable part, and `hello` has no room for more (217 of 221 bytes).
- **Invitation version 2 with prefix `LZ2-`.** Version 1 is refused; contacts paired with it stay as they are.
- **The accepting side sees the code only after `reveal`.** When pairing in person this takes seconds; the inviter's device is woken for the `hello` and answers without the person.
- **After the crypto review of the fix (2026-11-16):** no pairing without a valid `reveal`; a lost `reveal` is sent again for the same key; 24 hours without `reveal` ends the attempt; state is stored before sending, on both sides; the accepting side's key is new for every invitation; open version-1 invitations are deleted with the update; the codes are compared over another channel than the invitation's, and interfaces say so.

Clarifications made during the implementation of the pairing fix. They are implementation details and were not confirmed one by one:

- **Repeating `hello`:** While waiting for `reveal`, the accepting side sends the stored `hello` again unchanged, 5 minutes after the first sending and then each time the time since the first sending has passed once more (5, 10, 20, … minutes), until `reveal` arrives or the 24 hours and 10 minutes are over. If the inviter's mailbox is gone, the pairing has failed.
- **Answering a repeated `hello`:** The inviter answers every `hello` with the contact's key, even one whose identifier it has seen, with a new `reveal` message carrying the same `n`, as long as the person has not confirmed. A repeated `hello` with another key is dropped as seen; only its first arrival is shown as a warning.
- **Further `reveal`s:** After the first valid one, a `reveal` with the same `n` is ignored, one with another `n` triggers the alarm. After pairing, `reveal` is ignored.
- **`hello_ok` kept back:** If it completes the pairing together with `reveal`, the accepting side still shows the code, with the hint to remove the contact if it does not match.
- **After an alarm or a failed pairing** the contact stays on the accepting side as ended, like after a rejection, until the person removes it.
- **Pairings in progress from version 1** (no `n` stored) are ended at the next fetch like a pairing without `reveal`: `end`, mailbox deleted, "Kopplung fehlgeschlagen".
- **Several devices:** The handover and the reader registration of a new device leave out pairings in progress; they reach the other devices with `contact_part` once complete.
- **State file:** `n` is stored as `secret` (hex) in the pending invitation and in the contact, the accepting side's waiting state as `awaiting_reveal`; both are left out when empty, so stored paired contacts are unchanged. Pending invitations without `secret` are those of version 1.
- **Version byte:** An `LZ2-` invitation with version byte 1 gets the same hint as `LZ1-`; one with a higher version byte asks the person to update.

Clarifications made during the implementation of the relay (`crates/relay`). They are implementation details and were not confirmed one by one:

- **Order of checks:** If the token is missing or is not `b64url` of 32 bytes, the result is `401`. Then the mailbox is looked up; an unknown mailbox reports `404 unknown_mailbox` even if the token would not match, so that a removed client recognizes this reliably. Only then is the token compared (`401`).
- **Unreadable identifiers** in paths (not `b64url` or wrong length) result in `400 bad_request`. Hashes in `POST /v1/mailboxes` must be 64 hex digits; uppercase letters are tolerated.
- **Voucher:** It is passed like a token in the header `Authorization: Bearer …`. An unknown, redeemed or expired voucher results in `401`. It is valid for exactly 600 seconds.
- **Message length:** A body in `POST …/messages` that is not exactly 297 bytes, empty or longer included, results in `400 bad_request`, after the token check; the relay reads no more than one byte beyond the envelope. The `Content-Type` is not checked. Refused bodies do not count towards the rate limit.
- **Limits exceeded at the same time:** The rate limit is checked before the fill level, and `rate_limited` is returned. Rejected deliveries do not count. "Waiting" means: still stored, even if individual readers have already acknowledged it.
- **Rate limit only in memory:** The relay keeps the arrival times for the limit of 60 per hour only in memory and only for one hour. After a restart, counting starts anew.
- **Errors of the relay itself** report `500` with `{"error":"internal"}`.
- **Readers:** At most 16 readers per mailbox, above that `400 bad_request`, because every reader can trigger a wake signal. A newly registered reader starts with acknowledgment 0 and receives all waiting messages; a repeated `PUT` only changes the push address. A body `{}` counts as `{"push_url":null}`. When a reader is unregistered, the relay immediately deletes the messages that all remaining readers have acknowledged.
- **Push address:** at most 1024 bytes, only `https`, no port other than 443 (an explicit `:443` is allowed), without user name and password, not `localhost`, and as an IP address only public ones. The relay resolves the host name itself when waking and connects only to the checked public addresses (IPv4 and IPv6), so that a second resolution cannot return anything different. No proxy, no redirects, timeout 5 seconds. `LZ_PUSH_ALLOWED_HOSTS` compares the host name exactly, case-insensitively and without subdomains. Invalid addresses result in `400 bad_request` on registration.
- **Day of last use:** It is set by creating the mailbox, registering a reader, `subscribe` and `ack`, but not by delivery. A mailbox whose day is more than 90 days in the past is deleted. The relay cleans up every 5 minutes.
- **Acknowledgments:** `seq` is never reused, not even after messages are deleted. An acknowledgment beyond the highest assigned `seq` is capped to it so that it does not cover future messages. The relay does not respond to `ack`, except with an error.
- **WebSocket errors:** `code` is one of the error codes above (`bad_request`, `unauthorized`, `unknown_mailbox`, `unknown_reader`, `internal`). For an unreadable frame, the field `mailbox` is missing. `ack` for a mailbox that this connection has not subscribed to results in `bad_request`. If a subscribed mailbox is deleted or the reader is unregistered, the relay sends `unknown_mailbox` or `unknown_reader` respectively and ends the subscription. A repeated `subscribe` on the same connection to the same mailbox replaces the old subscription. At most 256 mailboxes per connection, frames at most 4 KiB.
- **Derived mailboxes and handovers (M5):** The read token of a mailbox created with the access token or derived results in `401` on `POST /v1/mailboxes`, like an unknown token. Mailboxes that existed before the update count as created with a voucher. In `PUT /v1/handovers/{handover}`, a missing `Lz-Fetch-Hash` means a reveal slot; `Lz-Reply-Hash` then results in `400`. For a handover slot the relay token is checked first (`401`), then the body (`413`, an empty body `400`), then whether the identifier or its reveal identifier is taken or reserved (`409`), then the limit per token and then the limit per voucher family (`429`), then the limit of all voucher families together and then that of the relay (`503`), both of which a `PUT` with `Lz-Reply-Hash` needs two places of. For a reveal slot: not reserved `404 unknown_handover`, filled already or an ordinary slot `409`, wrong token `401`, not exactly 32 bytes `400`. A reserved reveal slot that is not filled yet reports `404` on `GET`; a filled one expires with its reservation, 10 minutes after the reserving `PUT`. Malformed hex in the headers results in `400`.

Clarifications made during the implementation of the client and core (M5). They are implementation details and were not confirmed one by one:

- **Device list in the handover** includes the existing device E with its name, so that N knows all other devices; N drops its own entry if present.
- **`relay` is `null`** in the handover if E has no relay account.
- **Part count 0** in `contact_part` discards the transfer; counts above 4 are accepted as long as the parts fit together into a readable record.
- **`responded` before its request:** If `responded` arrives before the request itself, the request is recorded as answered at the time of the response; when the request arrives later it gets its real time and is not shown as open.
- **Rule stamps strictly increase per device:** A device's next `rules` has a later sending time or, in the same second, a larger identifier than its previous one, so that two changes in the same second are applied in order.
- **Device name:** At most 32 bytes, not empty. E sets its own name when adding a device (`lz device add … --name`); if the name changes while a group exists, E sends `device` again with the new name.
- **Sync mailbox without access token:** E tries the read tokens of its contact mailboxes one by one as authorisation (a voucher mailbox works, others give `401`, a full one `429 mailbox_quota`); if none works, adding fails with a clear message before anything is registered.
- **Token for handover slots:** E uses the access token of its relay account if the slots' relay is that relay, otherwise the read token of its sync mailbox, of a contact mailbox or of an open invitation on that relay, in this order.
- **Rollback when adding fails:** If adding fails after E created the group for it (handover slot taken, no `device` in time, relay unreachable), E removes the reader of N again and also deletes the sync mailbox and the group, so that nothing is left over. A crash of E while waiting leaves the reader of N registered; it is not repeated after a restart.
- **Polling:** Both devices look at the slots every 3 seconds until the expiry time of the device code; an unreachable relay is retried in that time. N on `401` when fetching a slot (the slot exists with another fetch hash) shows an alarm and starts over.
- **N's sequence after confirming:** N writes its state, sends its push address and then `device`; if `device` is not accepted before the expiry time, N deletes its state again.
- **Removal is detected** by `unknown_reader` for the sync mailbox on subscribing, or by `device_removed` naming this device; the device then deletes its state file and lock file. `unknown_reader` for contact mailboxes alone does not delete anything.
- **Order of fetching:** The sync mailbox is subscribed first, so that `requested` usually arrives before the contact's response to it. A response that arrives before `requested` is still discarded as a response to an unknown request; this is not specified further.
- **Which device:** Sync messages do not name their sender. A device shows the name of the other device only if there is exactly one.
- **Outbox:** Sending is retried at the start of every fetch, every 10 seconds while listening, and right after each own action; delivery stops at the first refusal to keep the order.
- **Removing a device** unregisters its reader on all own mailboxes including invitations and the sync mailbox; `device_removed` is sent only if all of them succeeded (an unknown reader or mailbox counts as success), otherwise the command can be repeated.

Not specified: the behavior when changing the relay.
