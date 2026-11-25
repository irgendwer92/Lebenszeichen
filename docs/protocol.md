# Lebenszeichen – Protocol (draft 2)

As of 2026-11-13. Belongs to `concept.md`.

Decided on 2026-11-13: multiple devices from the start, rules for requests including a weekly limit, no forward secrecy, visible removal. The rest is a proposal.

## Basic ideas

1. **One mailbox per contact and direction.** There is no global identifier for a person. Whoever removes a contact deletes that contact's mailbox; this technically locks the contact out.
2. **One key pair per contact.** Two contacts of the same person cannot tell by comparing keys that they know the same person.
3. **The relay is dumb.** It accepts ciphertext, passes it on and deletes it. It knows neither names nor keys nor message types.
4. **All messages are the same size.** Every message is padded to 256 bytes so that the relay cannot tell a request from a response by length.

## Terms

| Term | Meaning |
|---|---|
| Mailbox | Queue on a relay with a random identifier (32 bytes) |
| Read token | Secret with which the owner retrieves and deletes from their mailbox |
| Send token | Secret with which exactly one contact may deliver into the mailbox |
| Reader | A device of the mailbox owner, with its own identifier and push address |
| Contact key | X25519 key pair that is valid for one contact only |

## Pairing

B invites A. After that, both are equals and can request each other.

1. **B creates an invitation.** The app creates a mailbox on B's relay, generates a contact key and shows a QR code (alternatively as a text code for sending). Contents:
   - protocol version
   - relay address, mailbox identifier and send token of B's mailbox
   - B's public contact key
   - optionally a voucher with which A may create one mailbox on B's relay, once
   - a commitment to a secret value that B reveals only later

   The invitation is valid for 10 minutes and only once.
2. **A accepts.** A's app generates its own contact key and its own mailbox, on its own relay or, with the voucher, on B's relay. It sends `hello` to B's mailbox: A's public key as well as the address and send token of A's mailbox.
3. **Both see a verification code.** B's device answers the `hello` right away with `reveal`, the secret value from the commitment. Then both see six digits, derived from both public keys and that value. If the QR code was scanned directly from the screen, the comparison is unnecessary. Otherwise the digits are compared over another channel than the one the invitation took, for example in person or on the phone. If the invitation was sent, it protects against an attacker who swapped it on the way: because of the commitment, the attacker cannot search for keys that produce the same digits (since 2026-11-16).
4. **B confirms** and sends `hello_ok`. Both assign a name for the contact locally and exchange their `rules`.

If the verification code does not match, the side that notices rejects the pairing: it sends `end` and deletes its mailbox, and the other side sees "Kopplung abgelehnt" (decided on 2026-11-14). If B receives another `hello` for the same invitation after the first acceptance, B's app warns clearly: the code is then known to someone else, and the verification code must be compared with particular care.

Names are never transmitted. Each side names its contacts itself.

## Removing a contact

Removal is visible to the other side (decided).

1. B's app sends `end` to A and then deletes its own mailbox for this contact.
2. A's app shows "B hat die Verbindung beendet", disables the request button and deletes its own mailbox for B.
3. If `end` is lost, A's app notices the removal at the latest on the next request: the relay reports that the mailbox no longer exists.

Open requests in both directions expire in the process.

## Encryption

- `crypto_box` from libsodium (X25519, XSalsa20, Poly1305) between the two contact keys, with a random nonce per message.
- Layout of a message on the wire: version byte, nonce (24 bytes), ciphertext. The plaintext is padded to a fixed length before encryption.
- Replay protection: every message carries a timestamp and a random identifier; the receiver remembers identifiers until they expire and discards duplicates and expired messages.

**No forward secrecy (decided).** There is no ratchet. Anyone who later obtains a contact key can decrypt old recorded messages of that contact. Their content is a request, "✓" or a short reason text. In return, the protocol stays small, and multiple devices can use the same key. Remedy in case of suspicion: pair the contact again.

## Message types

### Between contacts

| Type | Fields | Purpose |
|---|---|---|
| `hello` | public key, mailbox address, send token | Accept pairing |
| `hello_ok` | – | Confirm pairing |
| `reveal` | secret value of the invitation | Lets the accepting side compute the verification code (since 2026-11-16) |
| `rules` | minimum interval, expiry time, weekly limit | The requested party states how often they may be asked |
| `request` | identifier, time | "Lebenszeichen?" |
| `response` | identifier of the request, kind (`manual` or `reported`), reason (only for `reported`), time | The sign of life |
| `end` | – | Contact was removed |

Clients ignore unknown types and unknown fields.

There is deliberately **no** delivery or read receipt. A only learns that the relay accepted the request.

### Between own devices

Own devices share a sync mailbox and a symmetric group key. Decided on 2026-11-16 for M5; the exact format is in `wire-format.md`.

| Type | Fields | Purpose |
|---|---|---|
| `device` | reader identifier, name | A new device announces itself |
| `device_removed` | reader identifier | A device was removed |
| `requested` | contact, identifier and time of the request | A request of the person's own from another device: disable the button and accept the response |
| `responded` | contact, identifier of the request, kind, reason, time | Withdraw the display on the other devices and start the minimum interval there |
| `rules_set` | contact, own rules | Rules changed on one device |
| `contact_part` | contact, part of its data | A contact paired on one device |
| `contact_removed` | contact | A contact ended on one device |
| `state_shared` | reason, end of the state | A reported state handed to the other devices (M8c) |
| `state_cleared` | – | The shared state ended (M8c) |
| `satellite_part` | satellite, part of its data | A satellite set up on one device (M10) |
| `satellite_settings` | satellite, its settings | Waiting time and long-press preset of a satellite changed (M10) |
| `satellite_contact_settings` | satellite, contact, its settings | Reported answers and ask button of a satellite for a contact changed (M10) |

The earlier plan had `counted` to keep counters equal. It is not needed: every device reads all mailboxes and counts the same requests.

### Satellite (M10)

A satellite (a small device in the hallway with a button and a display) reads the own contact mailboxes like a device, with a reader token of its own and the role `satellite`, but holds no group key, no sync mailbox and no relay access token. It talks to the full devices only through its **channel**: one mailbox and one symmetric key, form C. Types are one-way: the satellite sends only up types, the full devices only down types, and each side drops everything else. Details in `m10-satellite.md` and `wire-format.md`.

| Type | Direction | Purpose |
|---|---|---|
| `sat_answered` | up | The satellite answered a request (manual or reported), as `responded` |
| `sat_asked` | up | The satellite asked a contact, as `requested` |
| `sat_state_shared`, `sat_state_cleared` | up | Its shared state, as `state_shared` and `state_cleared` |
| `home_contact_part` | down | A contact for the satellite (one paired later) |
| `home_responded`, `home_requested`, `home_rules` | down | What a full device did, next to `responded`, `requested`, `rules_set` |
| `home_contact_settings`, `home_settings` | down | Its settings, set on a full device |
| `home_state_shared`, `home_state_cleared` | down | The person's shared state |
| `home_send_token` | down | The new send token of the channel, after a full device was removed |

## Rules for requests (decided)

The app should not become a surveillance tool. The requested party sets the rules per contact and enforces them on their devices. The requester's device knows them from `rules` and disables the button accordingly ("request possible again from 18:00").

| Rule | Default | Adjustable |
|---|---|---|
| Open requests per contact | at most one | no |
| Expiry time of an unanswered request | 24 hours | yes |
| Minimum interval after a response | 6 hours | yes |
| Requests per week | at most 7 | yes |
| Waiting time before a reported response | 15 minutes | yes, local only |

On the weekly limit:
- Counting is over the last 7 days on a rolling basis, not per calendar week.
- Every accepted request counts, even if it expires unanswered.
- The rules apply per contact. There is no limit across all contacts.

Requests that violate the rules are silently discarded by the receiving device. With an unmodified client this does not happen, because the button is disabled.

Evaluation of responses on the requester's side:
- At most one `reported` response is displayed per request.
- A `manual` response replaces an earlier `reported` response.
- Responses to expired or unknown requests are discarded.

## Multiple devices (decided: from the start)

- All devices of a person share contact keys and mailboxes. Contacts cannot see how many devices someone has.
- A new device is set up by QR code: the new device shows a code with a public key, the existing device seals the contact list, rules and group key to it and hands them over through the relay.
- Every device is its own reader; the relay holds messages until all readers have acknowledged.
- If the person reacts on one device, it sends `responded` to the others.
- A status report applies only to the device that received it. That device sends the `reported` response after the waiting time. Since M8c a report can be shared: the other devices then answer with it too, each where reported answers are allowed on it (`m8c-share-state.md`).
- A device is removed by another device unregistering it as a reader and ending the group of own devices; the remaining devices are added again to be linked. If a device is lost, the contacts must be paired again, because it knows all contact keys.
- A satellite (M10) is removed by any full device: its reader is revoked everywhere and its channel deleted, which it takes as the sign to delete its state; the group stays. Removing a full device keeps the satellite linked: the removed device is revoked on the channel too, and the send token of the channel is replaced.

Disadvantage: if one device is compromised, all contact keys are affected.

## Relay

### Interface

| Call | Authorization | Effect |
|---|---|---|
| Create mailbox | Access token of the relay, voucher, or the shared token or a full reader's token of a voucher mailbox (M5, M10) | Returns mailbox identifier; the client passes the hashes of the shared read token, or since M10 of its own reader token instead, and of the send token. A voucher mailbox may derive up to 10 further mailboxes. |
| Create voucher | Access token | Valid once, for exactly one mailbox |
| Deliver message | Send token | Exactly one envelope (297 bytes); clearly reports if the mailbox no longer exists |
| Register, change or unregister reader | Shared token or a full reader's token; a reader's own token for itself (M10) | Reader identifier, push address, since M10 the hash of the reader's own token and its role (full or satellite) |
| Retrieve messages (WebSocket) | The reader's own token, or the shared token for a reader without one; multiple mailboxes per connection | Delivery; the reader acknowledges each message |
| Delete mailbox | Shared token or a full reader's token | Immediately, including contents |
| Drop the shared token, replace the send token (M10) | A full reader's token | Readers without a token of their own lose access |
| Revoke all readers but a given set (M10) | A full reader's token | Every reader except the caller and the named ones is deleted, optionally with the shared token, in one step; used when removing a device, so readers it planted go too |
| Store and fetch a handover slot (M5) | Storing: access token, or the shared token or a full reader's token of any mailbox of the relay, for a reveal slot the reply token; fetching: fetch token | One-time block for adding a device, at most 64 KiB, at most 10 minutes |

When a new message arrives, the relay sends a contentless wake signal (UnifiedPush) to the push address of each reader. The device then connects itself and fetches the message.

### What the relay stores

| Data | Duration |
|---|---|
| Mailbox identifier, hash of the shared read token (until dropped, M10) and send token | until the mailbox is deleted |
| Readers per mailbox: identifier, push address, last acknowledgment, since M10 the hash of the reader's own token and its role | until the mailbox or reader is deleted |
| Messages: ciphertext and arrival time | until all readers have acknowledged, at most 24 hours |
| Open vouchers (hash) | until redeemed, at most 10 minutes |
| Day of last use per mailbox | for the cleanup rule |
| How a mailbox was created and, for a derived one, its voucher mailbox (M5); for a voucher mailbox, how many mailboxes it has derived (M9) | until the mailbox is deleted |
| Handover slots: content, hash of fetch or reply token, hash of the authorizing token, for a read token of a voucher family its voucher mailbox (M9) | until fetched, at most 10 minutes |

### What the relay does not store

- Names, keys, plaintext, message types
- IP addresses and access logs (must also be turned off in the reverse proxy)
- delivered messages or any history
- which mailboxes belong together

### What the operator can still see

Even without content, metadata remains if someone actively observes the relay:

- **Times:** when delivery into which mailbox takes place.
- **Association:** mailboxes that are retrieved over the same connection or have the same push address recognizably belong to the same device. A delivery into B's mailbox is often followed by one into A's mailbox; a contact pair can be derived from this.
- **IP addresses** during the connection.

With a self-operated relay, the operator is a trusted person. For a later release with third-party relays, this would be the most important point to sharpen.

- **Several devices (M5):** a sync mailbox is recognizable by its behavior, because its readers also deliver into it. Storing a handover links a mailbox of the person to the network address of the new device. Derived mailboxes cannot be withdrawn by the holder of the access token who issued the voucher.

- **Pairing (since 2026-11-16):** the inviter answers a `hello` right away with `reveal`. A delivery into the inviter's mailbox followed at once by one into the accepting side's mailbox links the two mailboxes and marks the moment of pairing.

The push service (ntfy) only sees when a device is woken.

### Protection against abuse

- Mailboxes only with access token, voucher, or derived from a voucher mailbox (at most 10 at a time and 30 in total each, not chainable).
- At most 64 handover slots on the relay and two per authorizing token. A voucher mailbox and the mailboxes derived from it form a voucher family; its read tokens share two slots, and all voucher families together at most 32, so that the holder of the access token can always add devices (since M9).
- Messages are exactly one envelope long, so the relay cannot carry other data (since M9).
- Wake signals go only to `https` on port 443 (since M9).
- At most 32 waiting messages and 60 deliveries per hour per mailbox (60 since 2026-11-17, because the own devices of a person share their sync mailbox and setting them up hit 20; 32 since 2026-11-16 for the sync mailbox of several devices; before that 8, decided on 2026-11-14; before that 6, which could block the `end` during normal use through pairing, rule changes and responses). The limit counts all messages because the relay does not know the types; it is not a substitute for the rules for requests.
- Mailboxes without retrieval for 90 days are deleted.

These limits only protect the relay. The rules for requests are enforced by the requested party's device, because the relay does not know the message types.

## Open questions

- Exact format of the invitation (encoding, length of the text code).
- Behavior when a contact changes relay.
- Recovery after loss of all devices (currently: pair all contacts again).
