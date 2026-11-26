/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <string.h>

#include "internal.h"
#include "lz/crypto.h"
#include "lz/relay.h"

/* ---- b64url and hex ---- */

static const char B64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

size_t lz_b64url_encode(char *out, size_t cap, const uint8_t *in, size_t len)
{
    size_t need = LZ_B64URL_LEN(len), i, o = 0;
    if (cap < need) return 0;
    for (i = 0; i + 3 <= len; i += 3) {
        uint32_t v = ((uint32_t)in[i] << 16) | ((uint32_t)in[i + 1] << 8) | in[i + 2];
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
        out[o++] = B64[(v >> 6) & 63];
        out[o++] = B64[v & 63];
    }
    if (len - i == 1) {
        uint32_t v = (uint32_t)in[i] << 16;
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
    } else if (len - i == 2) {
        uint32_t v = ((uint32_t)in[i] << 16) | ((uint32_t)in[i + 1] << 8);
        out[o++] = B64[(v >> 18) & 63];
        out[o++] = B64[(v >> 12) & 63];
        out[o++] = B64[(v >> 6) & 63];
    }
    if (o < cap) out[o] = '\0';
    return o;
}

static int b64_value(char c)
{
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '-') return 62;
    if (c == '_') return 63;
    return -1;
}

int lz_b64url_decode(uint8_t *out, size_t out_len, const char *in, size_t in_len)
{
    size_t i, o = 0;
    uint32_t acc = 0;
    int bits = 0;
    if (in_len != LZ_B64URL_LEN(out_len)) return LZ_E_VALUE;
    for (i = 0; i < in_len; i++) {
        int v = b64_value(in[i]);
        if (v < 0) return LZ_E_VALUE;
        acc = (acc << 6) | (uint32_t)v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out[o++] = (uint8_t)(acc >> bits);
            acc &= (1u << bits) - 1;
        }
    }
    /* Trailing bits must be zero: one encoding per value. */
    if (acc != 0 || o != out_len) {
        memset(out, 0, out_len);
        return LZ_E_VALUE;
    }
    return LZ_OK;
}

void lz_hex_encode(char *out, const uint8_t *in, size_t len)
{
    static const char H[] = "0123456789abcdef";
    size_t i;
    for (i = 0; i < len; i++) {
        out[2 * i] = H[in[i] >> 4];
        out[2 * i + 1] = H[in[i] & 15];
    }
}

/* ---- writing frames ---- */

typedef struct {
    char *p;
    size_t cap, pos;
    int ok;
} sbuf;

static void s_put(sbuf *s, const char *t, size_t n)
{
    if (!s->ok) return;
    if (n >= s->cap - s->pos) {
        s->ok = 0;
        return;
    }
    memcpy(s->p + s->pos, t, n);
    s->pos += n;
    s->p[s->pos] = '\0';
}

static void s_str(sbuf *s, const char *t) { s_put(s, t, strlen(t)); }

static void s_b64(sbuf *s, const uint8_t *in, size_t len)
{
    char tmp[LZ_B64URL_LEN(64) + 1];
    size_t n;
    if (len > 64) {
        s->ok = 0;
        return;
    }
    n = lz_b64url_encode(tmp, sizeof tmp, in, len);
    s_put(s, tmp, n);
    lz_wipe(tmp, sizeof tmp);
}

static void s_u64(sbuf *s, uint64_t v)
{
    char tmp[21];
    int i = 20;
    tmp[i] = '\0';
    do {
        tmp[--i] = (char)('0' + (int)(v % 10));
        v /= 10;
    } while (v);
    s_str(s, tmp + i);
}

static size_t s_done(sbuf *s)
{
    if (!s->ok) {
        if (s->cap) lz_wipe(s->p, s->cap);
        return 0;
    }
    return s->pos;
}

static void s_init(sbuf *s, char *out, size_t cap)
{
    s->p = out;
    s->cap = cap;
    s->pos = 0;
    s->ok = cap > 0;
    if (cap) out[0] = '\0';
}

size_t lz_frame_subscribe(char *out, size_t cap, const uint8_t mailbox[32],
                          const uint8_t token[32], const uint8_t reader[16])
{
    sbuf s;
    s_init(&s, out, cap);
    s_str(&s, "{\"op\":\"subscribe\",\"mailbox\":\"");
    s_b64(&s, mailbox, 32);
    s_str(&s, "\",\"token\":\"");
    s_b64(&s, token, 32);
    s_str(&s, "\",\"reader\":\"");
    s_b64(&s, reader, 16);
    s_str(&s, "\"}");
    return s_done(&s);
}

size_t lz_frame_ack(char *out, size_t cap, const uint8_t mailbox[32], uint64_t seq)
{
    sbuf s;
    s_init(&s, out, cap);
    s_str(&s, "{\"op\":\"ack\",\"mailbox\":\"");
    s_b64(&s, mailbox, 32);
    s_str(&s, "\",\"seq\":");
    s_u64(&s, seq);
    s_str(&s, "}");
    return s_done(&s);
}

size_t lz_body_token_hash(char *out, size_t cap, const uint8_t token[32])
{
    sbuf s;
    uint8_t h[32];
    char hex[64];
    lz_sha256(h, token, 32);
    lz_hex_encode(hex, h, 32);
    s_init(&s, out, cap);
    s_str(&s, "{\"token_hash\":\"");
    s_put(&s, hex, 64);
    s_str(&s, "\"}");
    return s_done(&s);
}

/* ---- reading frames ---- */

#define MAX_FRAME 4096
#define MAX_DEPTH 8

typedef struct {
    const char *p;
    size_t len, pos;
} jr;

typedef struct {
    size_t start, len; /* the raw content between the quotes */
    int escaped;       /* contains a backslash escape */
} jstr;

static void j_ws(jr *j)
{
    while (j->pos < j->len) {
        char c = j->p[j->pos];
        if (c != ' ' && c != '\t' && c != '\n' && c != '\r') break;
        j->pos++;
    }
}

static int j_peek(jr *j)
{
    j_ws(j);
    return j->pos < j->len ? (unsigned char)j->p[j->pos] : -1;
}

static int j_hex(char c)
{
    return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

static int j_string(jr *j, jstr *out)
{
    if (j_peek(j) != '"') return 0;
    j->pos++;
    out->start = j->pos;
    out->escaped = 0;
    while (j->pos < j->len) {
        unsigned char c = (unsigned char)j->p[j->pos];
        if (c == '"') {
            out->len = j->pos - out->start;
            j->pos++;
            return 1;
        }
        if (c < 0x20) return 0;
        if (c == '\\') {
            char e;
            out->escaped = 1;
            if (++j->pos >= j->len) return 0;
            e = j->p[j->pos];
            if (e == 'u') {
                int k;
                for (k = 0; k < 4; k++) {
                    if (++j->pos >= j->len || !j_hex(j->p[j->pos])) return 0;
                }
            } else if (!strchr("\"\\/bfnrt", e) || e == '\0') {
                return 0;
            }
        }
        j->pos++;
    }
    return 0;
}

static int j_digit(jr *j)
{
    return j->pos < j->len && j->p[j->pos] >= '0' && j->p[j->pos] <= '9';
}

static int j_number(jr *j)
{
    if (j_peek(j) == '-') j->pos++;
    if (!j_digit(j)) return 0;
    if (j->p[j->pos] == '0') {
        j->pos++;
    } else {
        while (j_digit(j)) j->pos++;
    }
    if (j->pos < j->len && j->p[j->pos] == '.') {
        j->pos++;
        if (!j_digit(j)) return 0;
        while (j_digit(j)) j->pos++;
    }
    if (j->pos < j->len && (j->p[j->pos] == 'e' || j->p[j->pos] == 'E')) {
        j->pos++;
        if (j->pos < j->len && (j->p[j->pos] == '+' || j->p[j->pos] == '-')) j->pos++;
        if (!j_digit(j)) return 0;
        while (j_digit(j)) j->pos++;
    }
    return 1;
}

static int j_word(jr *j, const char *w)
{
    size_t n = strlen(w);
    if (j->len - j->pos < n || memcmp(j->p + j->pos, w, n) != 0) return 0;
    j->pos += n;
    return 1;
}

static int j_skip(jr *j, int depth);

static int j_skip_container(jr *j, int depth, char close, int object)
{
    j->pos++;
    if (j_peek(j) == close) {
        j->pos++;
        return 1;
    }
    for (;;) {
        if (object) {
            jstr k;
            if (!j_string(j, &k) || j_peek(j) != ':') return 0;
            j->pos++;
        }
        if (!j_skip(j, depth + 1)) return 0;
        if (j_peek(j) == ',') {
            j->pos++;
            continue;
        }
        if (j_peek(j) == close) {
            j->pos++;
            return 1;
        }
        return 0;
    }
}

/* Skips one value of any type. */
static int j_skip(jr *j, int depth)
{
    jstr s;
    int c = j_peek(j);
    if (depth > MAX_DEPTH) return 0;
    switch (c) {
    case '"':
        return j_string(j, &s);
    case '{':
        return j_skip_container(j, depth, '}', 1);
    case '[':
        return j_skip_container(j, depth, ']', 0);
    case 't':
        return j_word(j, "true");
    case 'f':
        return j_word(j, "false");
    case 'n':
        return j_word(j, "null");
    default:
        return j_number(j);
    }
}

static int j_key_is(const jr *j, const jstr *k, const char *name)
{
    size_t n = strlen(name);
    return !k->escaped && k->len == n && memcmp(j->p + k->start, name, n) == 0;
}

/* An unsigned integer without sign, fraction or exponent. */
static int j_u64(jr *j, uint64_t *out)
{
    uint64_t v = 0;
    size_t start;
    if (j_peek(j) < '0' || j_peek(j) > '9') return 0;
    start = j->pos;
    while (j_digit(j)) {
        unsigned d = (unsigned)(j->p[j->pos] - '0');
        if (v > (UINT64_MAX - d) / 10) return 0;
        v = v * 10 + d;
        j->pos++;
    }
    if (j->p[start] == '0' && j->pos - start > 1) return 0;
    if (j->pos < j->len &&
        (j->p[j->pos] == '.' || j->p[j->pos] == 'e' || j->p[j->pos] == 'E')) {
        return 0;
    }
    *out = v;
    return 1;
}

/* Starts a top-level object; refuses input that is too long. */
static int j_open(jr *j, const char *text, size_t len)
{
    j->p = text;
    j->len = len;
    j->pos = 0;
    if (len > MAX_FRAME || j_peek(j) != '{') return 0;
    j->pos++;
    return 1;
}

/* After the closing brace only whitespace may follow. */
static int j_end(jr *j)
{
    j_ws(j);
    return j->pos == j->len;
}

/* Iterates the keys of the top-level object. Returns 1 with a key, 0 at
 * the end, -1 on malformed input. */
static int j_next_key(jr *j, int *first, jstr *k)
{
    int c = j_peek(j);
    if (*first) {
        *first = 0;
        if (c == '}') {
            j->pos++;
            return 0;
        }
    } else {
        if (c == '}') {
            j->pos++;
            return 0;
        }
        if (c != ',') return -1;
        j->pos++;
    }
    if (!j_string(j, k) || j_peek(j) != ':') return -1;
    j->pos++;
    return 1;
}

int lz_frame_parse(lz_frame *f, const char *text, size_t len)
{
    jr j;
    jstr k, op = {0, 0, 0}, mailbox = {0, 0, 0}, data = {0, 0, 0}, code = {0, 0, 0};
    int first = 1, r, have_op = 0, have_mailbox = 0, have_seq = 0, have_data = 0, have_code = 0;
    uint64_t seq = 0;

    memset(f, 0, sizeof *f);
    if (!j_open(&j, text, len)) return LZ_E_VALUE;
    while ((r = j_next_key(&j, &first, &k)) == 1) {
        jstr *target = NULL;
        int *have = NULL;
        if (j_key_is(&j, &k, "op")) {
            target = &op;
            have = &have_op;
        } else if (j_key_is(&j, &k, "mailbox")) {
            target = &mailbox;
            have = &have_mailbox;
        } else if (j_key_is(&j, &k, "data")) {
            target = &data;
            have = &have_data;
        } else if (j_key_is(&j, &k, "code")) {
            target = &code;
            have = &have_code;
        } else if (j_key_is(&j, &k, "seq")) {
            if (have_seq || !j_u64(&j, &seq)) return LZ_E_VALUE;
            have_seq = 1;
            continue;
        }
        if (target) {
            if (*have || !j_string(&j, target) || target->escaped) return LZ_E_VALUE;
            *have = 1;
        } else if (!j_skip(&j, 1)) {
            return LZ_E_VALUE;
        }
    }
    if (r < 0 || !j_end(&j) || !have_op) return LZ_E_VALUE;

    if (have_mailbox) {
        if (lz_b64url_decode(f->mailbox, 32, text + mailbox.start, mailbox.len) != LZ_OK) {
            return LZ_E_VALUE;
        }
        f->has_mailbox = 1;
    }
    if (j_key_is(&j, &op, "subscribed")) {
        if (!f->has_mailbox) return LZ_E_VALUE;
        f->op = LZ_FRAME_SUBSCRIBED;
    } else if (j_key_is(&j, &op, "message")) {
        if (!f->has_mailbox || !have_seq || !have_data) return LZ_E_VALUE;
        if (lz_b64url_decode(f->data, LZ_ENVELOPE_LEN, text + data.start, data.len) != LZ_OK) {
            return LZ_E_VALUE;
        }
        f->seq = seq;
        f->op = LZ_FRAME_MESSAGE;
    } else if (j_key_is(&j, &op, "error")) {
        size_t i;
        if (!have_code || code.len == 0 || code.len > LZ_MAX_ERROR_CODE) return LZ_E_VALUE;
        for (i = 0; i < code.len; i++) {
            char c = text[code.start + i];
            if (!((c >= 'a' && c <= 'z') || c == '_')) return LZ_E_VALUE;
            f->code[i] = c;
        }
        f->code[code.len] = '\0';
        f->op = LZ_FRAME_ERROR;
    } else {
        f->op = LZ_FRAME_OTHER;
    }
    return LZ_OK;
}

int lz_parse_error_body(char code[LZ_MAX_ERROR_CODE + 1], const char *text, size_t len)
{
    jr j;
    jstr k, v;
    int first = 1, r, found = 0;
    code[0] = '\0';
    if (!j_open(&j, text, len)) return LZ_E_VALUE;
    while ((r = j_next_key(&j, &first, &k)) == 1) {
        if (j_key_is(&j, &k, "error") && !found) {
            size_t i;
            if (!j_string(&j, &v) || v.escaped || v.len > LZ_MAX_ERROR_CODE) return LZ_E_VALUE;
            for (i = 0; i < v.len; i++) {
                char c = text[v.start + i];
                if (!((c >= 'a' && c <= 'z') || c == '_')) {
                    code[0] = '\0';
                    return LZ_E_VALUE;
                }
                code[i] = c;
            }
            code[v.len] = '\0';
            found = 1;
        } else if (!j_skip(&j, 1)) {
            code[0] = '\0';
            return LZ_E_VALUE;
        }
    }
    if (r < 0 || !j_end(&j)) {
        code[0] = '\0';
        return LZ_E_VALUE;
    }
    return LZ_OK;
}

int lz_health_has_reader_tokens(const char *text, size_t len)
{
    jr j;
    jstr k, v;
    int first = 1, r, found = 0;
    if (!j_open(&j, text, len)) return 0;
    while ((r = j_next_key(&j, &first, &k)) == 1) {
        if (j_key_is(&j, &k, "features") && j_peek(&j) == '[') {
            j.pos++;
            if (j_peek(&j) == ']') {
                j.pos++;
                continue;
            }
            for (;;) {
                if (j_peek(&j) == '"') {
                    if (!j_string(&j, &v)) return 0;
                    if (j_key_is(&j, &v, "reader_tokens")) found = 1;
                } else if (!j_skip(&j, 2)) {
                    return 0;
                }
                if (j_peek(&j) == ',') {
                    j.pos++;
                    continue;
                }
                if (j_peek(&j) != ']') return 0;
                j.pos++;
                break;
            }
        } else if (!j_skip(&j, 1)) {
            return 0;
        }
    }
    return r == 0 && j_end(&j) && found;
}

/* ---- addresses ---- */

int lz_url_ok(const lz_url *u)
{
    static const char https[] = "https://";
    static const char http[] = "http://";
    size_t host, i;
    if (u->len >= 8 && memcmp(u->b, https, 8) == 0) {
        host = 8;
    } else if (LZ_ALLOW_HTTP && u->len >= 7 && memcmp(u->b, http, 7) == 0) {
        host = 7;
    } else {
        return 0;
    }
    if (u->len == host || u->b[host] == '/' || u->b[u->len - 1] == '/') return 0;
    for (i = 0; i < u->len; i++) {
        uint8_t c = u->b[i];
        if (c <= 0x20 || c >= 0x7F || c == '"' || c == '\\') return 0;
    }
    return 1;
}

size_t lz_stream_url(char *out, size_t cap, const lz_url *relay)
{
    sbuf s;
    size_t skip;
    if (!lz_url_ok(relay)) return 0;
    s_init(&s, out, cap);
    if (relay->b[4] == 's') {
        s_str(&s, "wss://");
        skip = 8;
    } else {
        s_str(&s, "ws://");
        skip = 7;
    }
    s_put(&s, (const char *)relay->b + skip, relay->len - skip);
    s_str(&s, "/v1/stream");
    return s_done(&s);
}
