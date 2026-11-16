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

   The invitation is valid for 10 minutes and only once.
2. **A accepts.** A's app generates its own contact key and its own mailbox, on its own relay or, with the voucher, on B's relay. It sends `hello` to B's mailbox: A's public key as well as the address and send token of A's mailbox.
3. **Both see a verification code.** Six digits, derived from both public keys. If the QR code was scanned directly from the screen, the comparison is unnecessary. If the invitation was sent, it protects against an attacker who swapped it on the way.
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

**No forward secrecy (decided).** There is no ratchet. Anyone who later obtains a contact key can decrypt old recorded messages of that contact. Their content is "Anfrage", "✓" or a short reason text. In return, the protocol stays small, and multiple devices can use the same key. Remedy in case of suspicion: pair the contact again.

## Message types

### Between contacts

| Type | Fields | Purpose |
|---|---|---|
| `hello` | public key, mailbox address, send token | Accept pairing |
| `hello_ok` | – | Confirm pairing |
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

The earlier plan had `counted` to keep counters equal. It is not needed: every device reads all mailboxes and counts the same requests.

## Rules for requests (decided)

The app should not become a surveillance tool. The requested party sets the rules per contact and enforces them on their devices. The requester's device knows them from `rules` and disables the button accordingly ("wieder möglich ab 18:00").

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
- A status report applies only to the device that received it. That device sends the `reported` response after the waiting time.
- A device is removed by another device unregistering it as a reader. If a device is lost, the contacts must be paired again, because it knows all contact keys.

Disadvantage: if one device is compromised, all contact keys are affected.

## Relay

### Interface

| Call | Authorization | Effect |
|---|---|---|
| Create mailbox | Access token of the relay or voucher | Returns mailbox identifier; the client passes the hashes of read token and send token |
| Create voucher | Access token | Valid once, for exactly one mailbox |
| Deliver message | Send token | At most 512 bytes; clearly reports if the mailbox no longer exists |
| Register, change or unregister reader | Read token | Reader identifier and push address |
| Retrieve messages (WebSocket) | Read token, multiple mailboxes per connection | Delivery; the reader acknowledges each message |
| Delete mailbox | Read token | Immediately, including contents |

When a new message arrives, the relay sends a contentless wake signal (UnifiedPush) to the push address of each reader. The device then connects itself and fetches the message.

### What the relay stores

| Data | Duration |
|---|---|
| Mailbox identifier, hash of read token and send token | until the mailbox is deleted |
| Readers per mailbox: identifier, push address, last acknowledgment | until the mailbox or reader is deleted |
| Messages: ciphertext and arrival time | until all readers have acknowledged, at most 24 hours |
| Open vouchers (hash) | until redeemed, at most 10 minutes |
| Day of last use per mailbox | for the cleanup rule |

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

The push service (ntfy) only sees when a device is woken.

### Protection against abuse

- Mailboxes only with access token or voucher.
- At most 8 waiting messages and 20 deliveries per hour per mailbox (decided on 2026-11-14; previously 6, which could block the `end` during normal use through pairing, rule changes and responses). The limit counts all messages because the relay does not know the types; it is not a substitute for the rules for requests.
- Mailboxes without retrieval for 90 days are deleted.

These limits only protect the relay. The rules for requests are enforced by the requested party's device, because the relay does not know the message types.

## Open questions

- Exact format of the invitation (encoding, length of the text code).
- Behavior when a contact changes relay.
- Recovery after loss of all devices (currently: pair all contacts again).
