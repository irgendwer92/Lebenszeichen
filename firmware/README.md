# lz_core – the Lebenszeichen satellite in C

A portable C99 core for the satellite of `docs/m10-satellite.md` (part 2):
a small device at home (ESP32-C6 with an OLED, a button and a light) that
shows the requests of the person's contacts, answers them with a press,
answers with a reported state after the waiting time, shares states with
the person's full devices and can ask a contact itself.

The core does no I/O: no network, no flash, no `malloc`, no global mutable
state. Everything lives in one caller-owned `lz_ctx`. Time and randomness
come through callbacks, persistence through one blob the caller stores.
Thin adapters per SDK (ESP-IDF first, then Arduino, Pico SDK, MicroPython)
move bytes between it and the network, the flash and the buttons; they are
not part of this directory yet.

The binding specification is `docs/wire-format.md` ("Satellite (M10)",
envelopes, rules for requests, relay interface). The executable model it
follows is the Rust reference satellite, `SatState` and `RefSatellite` in
`crates/client/src/tests/reference.rs`.

## Layout

| Path | Content |
|---|---|
| `include/lz/lz.h` | The API: context, inputs, outputs, view, persistence |
| `include/lz/wire.h` | Envelopes A and C, plaintext, message bodies, contact record, setup |
| `include/lz/relay.h` | b64url, hex, the relay's JSON frames and HTTP bodies |
| `include/lz/crypto.h` | `crypto_box`/`crypto_secretbox` in libsodium's `_easy` layout, SHA-256 |
| `include/lz/config.h` | Compile-time limits |
| `src/` | The core (`wire.c`, `relay.c`, `ledger.c`, `satellite.c`, `io.c`, `store.c`, `crypto.c`, `sha256.c`) |
| `third_party/tweetnacl/` | TweetNaCl (public domain), the default crypto backend |
| `tests/` | Host tests (CTest), the fake relay, the Rust test vectors |

## Building on a host

```sh
cmake -S firmware -B build                 # gcc or clang, Debug, warnings as errors
cmake --build build
ctest --test-dir build --output-on-failure
```

Options: `-DLZ_CRYPTO=libsodium` (default `tweetnacl`),
`-DLZ_SHA256=mbedtls` (default `bundled`), `-DLZ_SANITIZE=ON`
(AddressSanitizer and UndefinedBehaviorSanitizer), `-DLZ_STRICT=OFF`,
`-DLZ_TESTS=OFF`. Where libsodium is installed, the tests compare the
TweetNaCl results with it.

In an SDK, compile `src/*.c` (and `third_party/tweetnacl/tweetnacl.c` unless
`LZ_CRYPTO_LIBSODIUM` is defined) with `include/`, `src/` and
`third_party/tweetnacl/` on the include path. Defines:

| Define | Effect |
|---|---|
| `LZ_CRYPTO_LIBSODIUM` | libsodium instead of TweetNaCl (ESP-IDF component `espressif/libsodium`, Arduino-ESP32) |
| `LZ_SHA256_MBEDTLS` | SHA-256 from mbedTLS through the PSA API (`psa_hash_*`; mbedTLS 3.x and 4.x / TF-PSA-Crypto, as in ESP-IDF 6) |
| `LZ_MAX_CONTACTS`, `LZ_MAX_RELAYS`, … | Limits, see `include/lz/config.h` |
| `LZ_ALLOW_HTTP=1` | Also accept `http://` relays (development only) |

## Test vectors

`tests/vectors/m10.json` is written by the Rust test
`vectors_are_reproducible` (`LZ_VECTORS_OUT=<file> cargo test -p lz-client
vectors_are_reproducible`). `tests/gen-vectors.sh` regenerates it,
`tests/gen-vectors.sh --check` fails if it is out of date (CI runs that in
the Rust job). The C test `vectors` replays the vector randomness and seals
every envelope again, byte for byte: every up and down type in form C, and
form A both ways between the satellite and a contact, with the direction
marker; it decodes and re-encodes every plaintext, the satellite contact
record and the setup, and checks the asking and waiting-time cases on the
installed setup.

## The API

```c
#include "lz/lz.h"

static lz_ctx ctx;                       /* ~56 KB with the defaults */

lz_platform pf = { user, now_fn, random_fn };
lz_init(&ctx, &pf);
if (lz_load(&ctx, blob, blob_len) != LZ_OK)  /* from NVS */
    lz_install(&ctx, setup, setup_len);      /* from BLE / USB serial */

/* whenever something happens: */
lz_online(&ctx, 1);                      /* WiFi up */
lz_connected(&ctx, relay, 1);            /* WebSocket open */
lz_received(&ctx, relay, text, len);     /* WebSocket text frame */
lz_http_response(&ctx, handle, status, date, body, len);
lz_press(&ctx); lz_long_press(&ctx); lz_ask(&ctx, 1);
lz_tick(&ctx);                           /* at least once a second */

/* after each input, drain the outputs in order: */
lz_output out;
while (lz_poll(&ctx, &out)) {
    switch (out.type) {
    case LZ_OUT_PERSIST:    /* lz_save(&ctx, write_fn, user) to NVS, now */
    case LZ_OUT_ERASE:      /* erase the stored state, close every connection */
    case LZ_OUT_CONNECT:    /* open a WebSocket to out.text (wss://…/v1/stream) as out.relay */
    case LZ_OUT_DISCONNECT: /* close out.relay */
    case LZ_OUT_SEND:       /* send text frame out.text/out.len on out.relay */
    case LZ_OUT_HTTP:       /* out.method out.url, Authorization: out.authorization,
                               Content-Type: out.content_type, body out.body/out.body_len;
                               answer with lz_http_response(out.handle, …) or lz_http_failed */
    case LZ_OUT_VIEW:       /* redraw from lz_view() */
    case LZ_OUT_LED:        /* out.led */
    case LZ_OUT_EVENT:      /* out.event: request opened/closed, answer, ask refused, … */
    }
}
```

Every input goes through `lz_feed(&ctx, &input)` with an `lz_input`
(type, relay, button, handle, status, date, data/len); the helpers above
wrap it. The same shape works from an ESP-IDF task, an
Arduino `loop()`, the Pico SDK's main loop, or a MicroPython native module
(one bytearray for the context, one method per input, `poll()` returning a
tuple).

### Rules for the adapter

- **Order.** Handle outputs in the order `lz_poll` returns them, and store
  `LZ_OUT_PERSIST` durably before handling the next one: an acknowledgement
  frame follows the state that reflects the message, a token replacement
  follows the stored new token, a delivery follows the stored outbox.
- **Lifetimes.** Pointers in `lz_output` stay valid until the next
  `lz_poll` or `lz_feed`. An adapter that sends HTTP asynchronously copies
  URL, header and body first.
- **One HTTP request at a time.** The core waits for the answer to a
  handle; it gives up on its own after 60 seconds.
- **Time.** `now` returns Unix seconds from NTP, or 0 before the first
  sync; the core does nothing time-based below 2024. Pass the `Date` header
  of every HTTP response as Unix seconds in `date`: more than 5 minutes off
  sets a warning (event, LED) and the core sends nothing until it agrees
  again (M9, decision 3). Messages are only delivered after one response
  with a `Date` header agreed.
- **Randomness.** `random` must come from the hardware RNG
  (`esp_fill_random` with the radio on, or the bootloader random source);
  a failure is reported, never papered over.
- **TLS and WebSocket.** HTTPS and WSS with certificate checks (a CA
  bundle). WebSocket pings every 30 seconds or so, so that the reverse proxy
  keeps the connection; the core handles reconnecting with backoff (connect
  when it says `LZ_OUT_CONNECT`, report `lz_connected(…, 0)` when a
  connection fails or closes).
- **Erase.** On `LZ_OUT_ERASE` erase the key partition (the satellite was
  removed on a full device, or a factory reset): `lz_factory_reset` for the
  10-second power-on hold.

### What the core does

- Installs a setup (`wire-format.md`, "Satellite setup"), refusing more
  contacts or relays than the build allows, duplicate contacts, unusable
  relay addresses and contact keys that are low-order points.
- Replaces every enrolment token with its own (`PUT readers/{own}` with
  `token_hash`), storing the new token before it is sent, retrying with the
  new token on `401` in case an earlier answer was lost; subscribes with
  its own token; acknowledges after processing.
- Opens form A from contacts (direction marker checked, replay protection
  and time window per contact) and applies the rules for requests as the
  requested party; shows open requests; a press answers every open request
  with a manual `response` and sends `sat_answered`.
- Opens form C from the channel, takes only down types (88–96) and drops
  everything else before remembering it; applies `home_*` as
  `wire-format.md` says (newest wins by stamp; `home_send_token` last
  received wins; `home_contact_part` reassembled, checked as `contact_part`;
  down messages about unknown contacts parked for 24 hours).
- Reported answers: two state slots (own and handed), the valid one set
  last applies; after the waiting time from the request's time, once per
  request, only for contacts that allow it; survives restarts because the
  timers follow from the stored request times.
- Asking: the requester's side of the rules with the contact's rules
  (open own request, minimum interval since the last manual answer, weekly
  limit); `request` and `sat_asked`; the answer is shown until the next
  press or 24 hours.
- Wipes everything on `unknown_reader` or `unknown_mailbox` for the
  channel; drops a contact on `unknown_mailbox` for its mailbox.

## Limits, sizes

Defaults in `include/lz/config.h`: 16 contacts, 3 relays, 16 requests per
contact and direction, 24 remembered identifiers per contact and 64 for the
channel, 16 outbox entries, 8 parked messages, 2 transfers.

Measured with the defaults (2026-11-26):

| | ESP32-C6 (riscv32-esp-elf-gcc, `-Os`, rv32imac) | x86-64 host (`-Os`) |
|---|---|---|
| Code (`.text`, everything incl. TweetNaCl and SHA-256) | 41.4 KB | 52.5 KB |
| `sizeof(lz_ctx)` | 57,408 B | 57,424 B |
| Largest stack frames | `lz_install` 2.2 KB, `lz_tweetnacl_scalarmult` 1.7 KB, `apply_part` 1.0 KB | |
| Persisted blob | at most 48.7 KB (`lz_save_max()`); a typical state of two contacts about 3 KB | |

The context is mostly contacts (2.6 KB each); fewer contacts shrink it
linearly. The adapter's task needs about 6 KB of stack for the core
(channel message → new contact → X25519); NVS needs a partition that holds
the largest blob.

## Security notes

- Every parser reads only inside its length, rejects truncated input,
  lengths beyond their bound, invalid UTF-8 and flags other than 0 and 1;
  the JSON reader takes only one object of at most 4 KiB with known keys of
  the right type, no duplicates, no escapes in the values it uses, depth at
  most 8. The tests cut every input at every length and fuzz all parsers
  under ASan and UBSan.
- Replay protection never lets a replay through: when a table is full the
  oldest identifier is forgotten and nothing sent at or before it is
  accepted any more.
- Secrets are wiped from stack buffers after use and from the context on
  removal; the only comparison of secret-derived values in the core (the
  blob's hash) is constant time. Poly1305 tags are verified in constant
  time by the backend.
- The blob's SHA-256 detects damage, not tampering: whoever can write the
  flash can read the keys in it. Flash and NVS encryption with secure boot
  are the recommended setup (`m10-satellite.md`, decision 4).
- A relay (or anyone who breaks TLS) can make the satellite wipe itself or
  drop a contact by sending `unknown_reader`/`unknown_mailbox`; it is
  trusted for availability anyway. It cannot make it accept anything: every
  message is authenticated end to end.

## License

LGPL-3.0-or-later (`LICENSE`, which builds on `LICENSE.GPL-3.0`) with a
static linking exception (`LICENSE-EXCEPTION`, **draft**, legal review
open, `docs/license-exception.md`). Sources carry
`SPDX-License-Identifier: LGPL-3.0-or-later WITH
LicenseRef-Lebenszeichen-static-linking-exception`. `third_party/tweetnacl/`
is in the public domain.
