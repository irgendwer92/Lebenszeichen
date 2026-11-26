/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The relay protocol as data (wire-format.md, "Relay interface" and
 * "Delivery"): b64url and hex, the WebSocket JSON frames the satellite sends
 * (subscribe, ack) and reads (subscribed, message, error), and the bodies of
 * the HTTP calls it makes. A tiny hand-written reader and writer for
 * exactly these frames; no general JSON.
 */
#ifndef LZ_RELAY_H
#define LZ_RELAY_H

#include <stddef.h>
#include <stdint.h>

#include "lz/wire.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Base64 with the URL alphabet, no padding. Returns the length written, or
 * 0 if cap is too small. */
size_t lz_b64url_encode(char *out, size_t cap, const uint8_t *in, size_t len);
/* Decodes exactly out_len bytes; refuses other lengths, characters outside
 * the alphabet, padding and non-zero trailing bits. */
int lz_b64url_decode(uint8_t *out, size_t out_len, const char *in, size_t in_len);
#define LZ_B64URL_LEN(n) (((n) * 4 + 2) / 3)

/* Lowercase hex. out needs 2 * len bytes; no terminator. */
void lz_hex_encode(char *out, const uint8_t *in, size_t len);

/* Frames. Each returns the length written (without terminator; out is
 * terminated if there is room), or 0 if cap is too small. */
size_t lz_frame_subscribe(char *out, size_t cap, const uint8_t mailbox[32],
                          const uint8_t token[32], const uint8_t reader[16]);
size_t lz_frame_ack(char *out, size_t cap, const uint8_t mailbox[32], uint64_t seq);

/* Body of PUT /v1/mailboxes/{m}/readers/{r} that replaces the token:
 * {"token_hash":"<hex of SHA-256(token)>"}. */
size_t lz_body_token_hash(char *out, size_t cap, const uint8_t token[32]);

/* Frames from the relay. */
enum {
    LZ_FRAME_OTHER = 0, /* an op the satellite does not use */
    LZ_FRAME_SUBSCRIBED = 1,
    LZ_FRAME_MESSAGE = 2,
    LZ_FRAME_ERROR = 3
};

#define LZ_MAX_ERROR_CODE 32

typedef struct {
    int op;
    uint8_t has_mailbox;
    uint8_t mailbox[32];
    uint64_t seq;                     /* message */
    uint8_t data[LZ_ENVELOPE_LEN];    /* message: the envelope */
    char code[LZ_MAX_ERROR_CODE + 1]; /* error, terminated */
} lz_frame;

/* Parses one text frame of at most 4096 bytes. LZ_OK with op set, or an
 * error for anything malformed: not one JSON object, a duplicate key, a
 * known key with the wrong type, a message without mailbox, seq or a data
 * field of exactly one envelope. Unknown keys are skipped. */
int lz_frame_parse(lz_frame *f, const char *text, size_t len);

/* Reads {"error":"<code>"} from an HTTP error body; code is terminated,
 * empty if there is none. */
int lz_parse_error_body(char code[LZ_MAX_ERROR_CODE + 1], const char *text, size_t len);

/* Whether a GET /v1/health body lists the feature reader_tokens. */
int lz_health_has_reader_tokens(const char *text, size_t len);

/* The WebSocket address for a relay address: https:// becomes wss://, then
 * /v1/stream. 0 if the address is not usable (see lz_url_ok). */
size_t lz_stream_url(char *out, size_t cap, const lz_url *relay);

/* Whether a relay address is usable: https:// (or http:// with
 * LZ_ALLOW_HTTP), a host, printable ASCII without spaces, quotes or
 * backslashes, no trailing slash. */
int lz_url_ok(const lz_url *relay);

#ifdef __cplusplus
}
#endif

#endif
