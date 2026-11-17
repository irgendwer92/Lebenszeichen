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

Gaps found are fixed in M9 and listed here.

## Work

- Relay image job in `container.yml`.
- `deploy/relay/` and `docs/self-hosting.md`.
- Clock offset warning in `lz-client`, app banner, `lz` output, texts in both
  languages.
- Behaviour tests and fixes.
- Review brief, Opus review, fixes.
- Dry run of the guide, then the test record.

## Test record

(follows)
