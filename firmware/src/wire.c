/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <string.h>

#include "internal.h"
#include "lz/crypto.h"

const lz_rules LZ_RULES_DEFAULT = {360, 1440, 7};
const lz_settings LZ_SETTINGS_DEFAULT = {15, 0, 0, {0, {0}}};

/* ---- reader and writer ---- */

void lz_rd_init(lz_rd *r, const uint8_t *p, size_t len)
{
    r->p = p;
    r->len = len;
    r->err = LZ_OK;
}

const uint8_t *lz_rd_take(lz_rd *r, size_t n)
{
    const uint8_t *p;
    if (r->err != LZ_OK) return NULL;
    if (n > r->len) {
        r->err = LZ_E_TRUNCATED;
        return NULL;
    }
    p = r->p;
    r->p += n;
    r->len -= n;
    return p;
}

void lz_rd_bytes(lz_rd *r, uint8_t *out, size_t n)
{
    const uint8_t *p = lz_rd_take(r, n);
    if (p) {
        memcpy(out, p, n);
    } else {
        memset(out, 0, n);
    }
}

uint8_t lz_rd_u8(lz_rd *r)
{
    const uint8_t *p = lz_rd_take(r, 1);
    return p ? p[0] : 0;
}

uint16_t lz_rd_u16(lz_rd *r)
{
    const uint8_t *p = lz_rd_take(r, 2);
    if (!p) return 0;
    return (uint16_t)(((unsigned)p[0] << 8) | p[1]);
}

uint32_t lz_rd_u32(lz_rd *r)
{
    const uint8_t *p = lz_rd_take(r, 4);
    return p ? lz_get_u32(p) : 0;
}

uint64_t lz_rd_u64(lz_rd *r)
{
    const uint8_t *p = lz_rd_take(r, 8);
    return p ? lz_get_u64(p) : 0;
}

void lz_rd_text(lz_rd *r, uint8_t *len_out, uint8_t *out, size_t max)
{
    uint8_t len = lz_rd_u8(r);
    const uint8_t *p = lz_rd_take(r, len);
    *len_out = 0;
    if (!p) return;
    if (len > max) {
        r->err = LZ_E_TOO_LONG;
        return;
    }
    if (!lz_utf8_valid(p, len)) {
        r->err = LZ_E_UTF8;
        return;
    }
    memcpy(out, p, len);
    *len_out = len;
}

uint8_t lz_rd_flag(lz_rd *r)
{
    uint8_t v = lz_rd_u8(r);
    if (r->err == LZ_OK && v > 1) r->err = LZ_E_VALUE;
    return v;
}

void lz_wr_init(lz_wr *w, uint8_t *p, size_t cap)
{
    w->p = p;
    w->cap = cap;
    w->pos = 0;
    w->err = LZ_OK;
}

void lz_wr_bytes(lz_wr *w, const uint8_t *b, size_t n)
{
    if (w->err != LZ_OK) return;
    if (n > w->cap - w->pos) {
        w->err = LZ_E_TOO_LONG;
        return;
    }
    if (n) memcpy(w->p + w->pos, b, n);
    w->pos += n;
}

void lz_wr_u8(lz_wr *w, uint8_t v) { lz_wr_bytes(w, &v, 1); }

void lz_wr_u16(lz_wr *w, uint16_t v)
{
    uint8_t b[2];
    b[0] = (uint8_t)(v >> 8);
    b[1] = (uint8_t)v;
    lz_wr_bytes(w, b, 2);
}

void lz_wr_u32(lz_wr *w, uint32_t v)
{
    uint8_t b[4];
    lz_put_u32(b, v);
    lz_wr_bytes(w, b, 4);
}

void lz_wr_u64(lz_wr *w, uint64_t v)
{
    uint8_t b[8];
    lz_put_u64(b, v);
    lz_wr_bytes(w, b, 8);
}

void lz_wr_text(lz_wr *w, uint8_t len, const uint8_t *b)
{
    lz_wr_u8(w, len);
    lz_wr_bytes(w, b, len);
}

uint32_t lz_get_u32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3];
}

uint64_t lz_get_u64(const uint8_t *p)
{
    return ((uint64_t)lz_get_u32(p) << 32) | lz_get_u32(p + 4);
}

void lz_put_u32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

void lz_put_u64(uint8_t *p, uint64_t v)
{
    lz_put_u32(p, (uint32_t)(v >> 32));
    lz_put_u32(p + 4, (uint32_t)v);
}

/* ---- UTF-8 ---- */

int lz_utf8_valid(const uint8_t *s, size_t len)
{
    size_t i = 0;
    while (i < len) {
        uint8_t c = s[i];
        size_t n;
        uint32_t cp, min;
        if (c < 0x80) {
            i++;
            continue;
        } else if (c >= 0xC2 && c <= 0xDF) {
            n = 1;
            cp = c & 0x1F;
            min = 0x80;
        } else if (c >= 0xE0 && c <= 0xEF) {
            n = 2;
            cp = c & 0x0F;
            min = 0x800;
        } else if (c >= 0xF0 && c <= 0xF4) {
            n = 3;
            cp = c & 0x07;
            min = 0x10000;
        } else {
            return 0;
        }
        if (len - i - 1 < n) return 0;
        {
            size_t k;
            for (k = 1; k <= n; k++) {
                uint8_t d = s[i + k];
                if ((d & 0xC0) != 0x80) return 0;
                cp = (cp << 6) | (d & 0x3F);
            }
        }
        if (cp < min || cp > 0x10FFFF || (cp >= 0xD800 && cp <= 0xDFFF)) return 0;
        i += n + 1;
    }
    return 1;
}

/* ---- message bodies ---- */

int lz_type_known(uint8_t t)
{
    return (t >= LZ_T_RULES && t <= LZ_T_END) || lz_type_is_up(t) || lz_type_is_down(t);
}

int lz_type_is_up(uint8_t t) { return t >= LZ_T_SAT_ANSWERED && t <= LZ_T_SAT_STATE_CLEARED; }

int lz_type_is_down(uint8_t t)
{
    return t >= LZ_T_HOME_CONTACT_PART && t <= LZ_T_HOME_SEND_TOKEN;
}

void lz_rd_rules(lz_rd *r, lz_rules *rules)
{
    rules->min_interval_minutes = lz_rd_u32(r);
    rules->expiry_minutes = lz_rd_u32(r);
    rules->weekly_limit = lz_rd_u16(r);
}

void lz_wr_rules(lz_wr *w, const lz_rules *rules)
{
    lz_wr_u32(w, rules->min_interval_minutes);
    lz_wr_u32(w, rules->expiry_minutes);
    lz_wr_u16(w, rules->weekly_limit);
}

void lz_rd_stamp(lz_rd *r, lz_stamp *s)
{
    s->at = lz_rd_u64(r);
    lz_rd_bytes(r, s->id, 16);
}

void lz_wr_stamp(lz_wr *w, const lz_stamp *s)
{
    lz_wr_u64(w, s->at);
    lz_wr_bytes(w, s->id, 16);
}

void lz_rd_settings(lz_rd *r, lz_settings *s)
{
    s->waiting_minutes = lz_rd_u16(r);
    s->preset_minutes = lz_rd_u32(r);
    s->share = lz_rd_flag(r);
    lz_rd_text(r, &s->reason.len, s->reason.b, LZ_MAX_REASON_LEN);
    if (s->preset_minutes == 0) {
        /* No preset: share and reason mean nothing. */
        s->share = 0;
        s->reason.len = 0;
    }
}

void lz_wr_settings(lz_wr *w, const lz_settings *s)
{
    lz_wr_u16(w, s->waiting_minutes);
    if (s->preset_minutes == 0) {
        static const uint8_t none[6] = {0};
        lz_wr_bytes(w, none, 6);
        return;
    }
    lz_wr_u32(w, s->preset_minutes);
    lz_wr_u8(w, s->share ? 1 : 0);
    lz_wr_text(w, s->reason.len, s->reason.b);
}

/* A kind byte and a reason; a manual response's reason is read (it must
 * be UTF-8) and dropped, as in the Rust implementation. */
static void rd_kind(lz_rd *r, uint8_t *kind, lz_reason *reason)
{
    uint8_t ignored[255];
    uint8_t ignored_len;
    *kind = lz_rd_u8(r);
    reason->len = 0;
    if (r->err != LZ_OK) return;
    if (*kind == LZ_KIND_MANUAL) {
        lz_rd_text(r, &ignored_len, ignored, sizeof ignored);
    } else if (*kind == LZ_KIND_REPORTED) {
        lz_rd_text(r, &reason->len, reason->b, LZ_MAX_REASON_LEN);
    } else {
        r->err = LZ_E_VALUE;
    }
}

static void wr_kind(lz_wr *w, uint8_t kind, const lz_reason *reason)
{
    if (kind == LZ_KIND_MANUAL) {
        lz_wr_u8(w, LZ_KIND_MANUAL);
        lz_wr_u8(w, 0);
    } else if (kind == LZ_KIND_REPORTED) {
        lz_wr_u8(w, LZ_KIND_REPORTED);
        lz_wr_text(w, reason->len, reason->b);
    } else {
        w->err = LZ_E_VALUE;
    }
}

static void rd_body(lz_rd *r, lz_message *m)
{
    switch (m->type) {
    case LZ_T_RULES:
        lz_rd_rules(r, &m->u.rules);
        break;
    case LZ_T_REQUEST:
    case LZ_T_END:
    case LZ_T_SAT_STATE_CLEARED:
    case LZ_T_HOME_STATE_CLEARED:
        break;
    case LZ_T_RESPONSE:
        lz_rd_bytes(r, m->u.response.request, 16);
        rd_kind(r, &m->u.response.kind, &m->u.response.reason);
        break;
    case LZ_T_SAT_ANSWERED:
    case LZ_T_HOME_RESPONDED:
        lz_rd_bytes(r, m->u.responded.contact, 32);
        lz_rd_bytes(r, m->u.responded.request, 16);
        rd_kind(r, &m->u.responded.kind, &m->u.responded.reason);
        m->u.responded.at = lz_rd_u64(r);
        break;
    case LZ_T_SAT_ASKED:
    case LZ_T_HOME_REQUESTED:
        lz_rd_bytes(r, m->u.requested.contact, 32);
        lz_rd_bytes(r, m->u.requested.request, 16);
        m->u.requested.at = lz_rd_u64(r);
        break;
    case LZ_T_SAT_STATE_SHARED:
    case LZ_T_HOME_STATE_SHARED:
        lz_rd_text(r, &m->u.state.reason.len, m->u.state.reason.b, LZ_MAX_REASON_LEN);
        m->u.state.until = lz_rd_u64(r);
        break;
    case LZ_T_HOME_CONTACT_PART: {
        const uint8_t *data;
        lz_rd_bytes(r, m->u.part.contact, 32);
        lz_rd_bytes(r, m->u.part.transfer, 16);
        m->u.part.index = lz_rd_u8(r);
        m->u.part.count = lz_rd_u8(r);
        m->u.part.len = lz_rd_u8(r);
        data = lz_rd_take(r, m->u.part.len);
        if (data && m->u.part.len > LZ_MAX_PART_LEN) r->err = LZ_E_TOO_LONG;
        if (r->err == LZ_OK) {
            memcpy(m->u.part.data, data, m->u.part.len);
        } else {
            m->u.part.len = 0;
        }
        break;
    }
    case LZ_T_HOME_RULES:
        lz_rd_bytes(r, m->u.rules_set.contact, 32);
        lz_rd_rules(r, &m->u.rules_set.rules);
        break;
    case LZ_T_HOME_CONTACT_SETTINGS:
        lz_rd_bytes(r, m->u.contact_settings.contact, 32);
        m->u.contact_settings.flags.reported = lz_rd_flag(r);
        m->u.contact_settings.flags.ask_button = lz_rd_u8(r);
        break;
    case LZ_T_HOME_SETTINGS:
        lz_rd_settings(r, &m->u.settings);
        break;
    case LZ_T_HOME_SEND_TOKEN:
        lz_rd_bytes(r, m->u.send_token, 32);
        break;
    default:
        /* Not parsed: the satellite drops these types. */
        break;
    }
}

static void wr_body(lz_wr *w, const lz_message *m)
{
    switch (m->type) {
    case LZ_T_RULES:
        lz_wr_rules(w, &m->u.rules);
        break;
    case LZ_T_REQUEST:
    case LZ_T_END:
    case LZ_T_SAT_STATE_CLEARED:
    case LZ_T_HOME_STATE_CLEARED:
        break;
    case LZ_T_RESPONSE:
        lz_wr_bytes(w, m->u.response.request, 16);
        wr_kind(w, m->u.response.kind, &m->u.response.reason);
        break;
    case LZ_T_SAT_ANSWERED:
    case LZ_T_HOME_RESPONDED:
        lz_wr_bytes(w, m->u.responded.contact, 32);
        lz_wr_bytes(w, m->u.responded.request, 16);
        wr_kind(w, m->u.responded.kind, &m->u.responded.reason);
        lz_wr_u64(w, m->u.responded.at);
        break;
    case LZ_T_SAT_ASKED:
    case LZ_T_HOME_REQUESTED:
        lz_wr_bytes(w, m->u.requested.contact, 32);
        lz_wr_bytes(w, m->u.requested.request, 16);
        lz_wr_u64(w, m->u.requested.at);
        break;
    case LZ_T_SAT_STATE_SHARED:
    case LZ_T_HOME_STATE_SHARED:
        lz_wr_text(w, m->u.state.reason.len, m->u.state.reason.b);
        lz_wr_u64(w, m->u.state.until);
        break;
    case LZ_T_HOME_CONTACT_PART:
        if (m->u.part.len > LZ_MAX_PART_LEN) {
            w->err = LZ_E_TOO_LONG;
            break;
        }
        lz_wr_bytes(w, m->u.part.contact, 32);
        lz_wr_bytes(w, m->u.part.transfer, 16);
        lz_wr_u8(w, m->u.part.index);
        lz_wr_u8(w, m->u.part.count);
        lz_wr_u8(w, m->u.part.len);
        lz_wr_bytes(w, m->u.part.data, m->u.part.len);
        break;
    case LZ_T_HOME_RULES:
        lz_wr_bytes(w, m->u.rules_set.contact, 32);
        lz_wr_rules(w, &m->u.rules_set.rules);
        break;
    case LZ_T_HOME_CONTACT_SETTINGS:
        lz_wr_bytes(w, m->u.contact_settings.contact, 32);
        lz_wr_u8(w, m->u.contact_settings.flags.reported ? 1 : 0);
        lz_wr_u8(w, m->u.contact_settings.flags.ask_button);
        break;
    case LZ_T_HOME_SETTINGS:
        lz_wr_settings(w, &m->u.settings);
        break;
    case LZ_T_HOME_SEND_TOKEN:
        lz_wr_bytes(w, m->u.send_token, 32);
        break;
    default:
        w->err = LZ_E_VALUE;
        break;
    }
}

int lz_message_encode(const lz_message *m, uint8_t out[LZ_PLAINTEXT_LEN])
{
    lz_wr w;
    memset(out, 0, LZ_PLAINTEXT_LEN);
    lz_wr_init(&w, out + LZ_HEADER_LEN, LZ_PLAINTEXT_LEN - LZ_HEADER_LEN);
    wr_body(&w, m);
    if (w.err != LZ_OK) {
        memset(out, 0, LZ_PLAINTEXT_LEN);
        return w.err;
    }
    out[0] = m->type;
    memcpy(out + 1, m->id, 16);
    lz_put_u64(out + 17, m->sent_at);
    out[25] = (uint8_t)(w.pos >> 8);
    out[26] = (uint8_t)w.pos;
    return LZ_OK;
}

int lz_message_decode(lz_message *m, const uint8_t *plaintext, size_t len)
{
    lz_rd r, body;
    uint16_t body_len;
    const uint8_t *b;
    memset(m, 0, sizeof *m);
    lz_rd_init(&r, plaintext, len);
    m->type = lz_rd_u8(&r);
    lz_rd_bytes(&r, m->id, 16);
    m->sent_at = lz_rd_u64(&r);
    body_len = lz_rd_u16(&r);
    b = lz_rd_take(&r, body_len);
    if (r.err != LZ_OK) {
        int err = r.err;
        memset(m, 0, sizeof *m);
        return err;
    }
    lz_rd_init(&body, b, body_len);
    rd_body(&body, m);
    if (body.err != LZ_OK) {
        memset(m, 0, sizeof *m);
        return body.err;
    }
    return LZ_OK;
}

int lz_stamp_cmp(const lz_stamp *a, const lz_stamp *b)
{
    if (a->at != b->at) return a->at < b->at ? -1 : 1;
    return memcmp(a->id, b->id, 16);
}

/* ---- envelopes ---- */

#define NONCE_AT 1
#define TAG_AT (1 + 24)

int lz_seal_plain(uint8_t env[LZ_ENVELOPE_LEN], const uint8_t plaintext[LZ_PLAINTEXT_LEN],
                  const uint8_t key[32], const uint8_t nonce[24])
{
    env[0] = LZ_PROTOCOL_VERSION;
    memcpy(env + NONCE_AT, nonce, 24);
    if (lz_secretbox_easy(env + TAG_AT, plaintext, LZ_PLAINTEXT_LEN, nonce, key) != 0) {
        memset(env, 0, LZ_ENVELOPE_LEN);
        return LZ_E_CRYPTO;
    }
    return LZ_OK;
}

int lz_open_plain(uint8_t plaintext[LZ_PLAINTEXT_LEN], const uint8_t *env, size_t len,
                  const uint8_t key[32])
{
    if (len < LZ_ENVELOPE_LEN) return LZ_E_TRUNCATED;
    if (len > LZ_ENVELOPE_LEN) return LZ_E_TOO_LONG;
    if (env[0] != LZ_PROTOCOL_VERSION) return LZ_E_VERSION;
    if (lz_secretbox_open_easy(plaintext, env + TAG_AT, LZ_ENVELOPE_LEN - TAG_AT, env + NONCE_AT,
                               key) != 0) {
        return LZ_E_DECRYPT;
    }
    return LZ_OK;
}

static uint8_t direction(const uint8_t sender[32], const uint8_t recipient[32])
{
    return memcmp(sender, recipient, 32) < 0 ? 1 : 2;
}

int lz_seal_a(uint8_t env[LZ_ENVELOPE_LEN], const lz_message *m, const uint8_t shared[32],
              const uint8_t own_pk[32], const uint8_t peer_pk[32], const uint8_t nonce[24])
{
    uint8_t plain[LZ_PLAINTEXT_LEN];
    int err = lz_message_encode(m, plain);
    if (err != LZ_OK) return err;
    if (plain[LZ_PLAINTEXT_LEN - 1] != 0) {
        lz_wipe(plain, sizeof plain);
        return LZ_E_TOO_LONG;
    }
    plain[LZ_PLAINTEXT_LEN - 1] = direction(own_pk, peer_pk);
    err = lz_seal_plain(env, plain, shared, nonce);
    lz_wipe(plain, sizeof plain);
    return err;
}

int lz_open_a(lz_message *m, const uint8_t *env, size_t len, const uint8_t shared[32],
              const uint8_t own_pk[32], const uint8_t peer_pk[32])
{
    uint8_t plain[LZ_PLAINTEXT_LEN];
    uint8_t marker;
    int err = lz_open_plain(plain, env, len, shared);
    memset(m, 0, sizeof *m);
    if (err != LZ_OK) return err;
    marker = plain[LZ_PLAINTEXT_LEN - 1];
    if (marker != 0 && marker != direction(peer_pk, own_pk)) {
        lz_wipe(plain, sizeof plain);
        return LZ_E_DECRYPT;
    }
    err = lz_message_decode(m, plain, sizeof plain);
    lz_wipe(plain, sizeof plain);
    return err;
}

int lz_seal_c(uint8_t env[LZ_ENVELOPE_LEN], const lz_message *m, const uint8_t key[32],
              const uint8_t nonce[24])
{
    uint8_t plain[LZ_PLAINTEXT_LEN];
    int err = lz_message_encode(m, plain);
    if (err == LZ_OK) err = lz_seal_plain(env, plain, key, nonce);
    lz_wipe(plain, sizeof plain);
    return err;
}

int lz_open_c(lz_message *m, const uint8_t *env, size_t len, const uint8_t key[32])
{
    uint8_t plain[LZ_PLAINTEXT_LEN];
    int err = lz_open_plain(plain, env, len, key);
    memset(m, 0, sizeof *m);
    if (err == LZ_OK) err = lz_message_decode(m, plain, sizeof plain);
    lz_wipe(plain, sizeof plain);
    return err;
}

/* ---- satellite contact record ---- */

int lz_contact_record_decode(lz_contact_record *c, const uint8_t *in, size_t len)
{
    lz_rd r;
    memset(c, 0, sizeof *c);
    lz_rd_init(&r, in, len);
    lz_rd_bytes(&r, c->secret_key, 32);
    lz_rd_bytes(&r, c->peer_key, 32);
    lz_rd_bytes(&r, c->enrolment, 32);
    lz_rd_text(&r, &c->own_relay.len, c->own_relay.b, LZ_MAX_URL_LEN);
    lz_rd_bytes(&r, c->peer_mailbox, 32);
    lz_rd_bytes(&r, c->peer_send_token, 32);
    lz_rd_text(&r, &c->peer_relay.len, c->peer_relay.b, LZ_MAX_URL_LEN);
    lz_rd_rules(&r, &c->own_rules);
    lz_rd_stamp(&r, &c->own_rules_stamp);
    lz_rd_rules(&r, &c->peer_rules);
    lz_rd_stamp(&r, &c->peer_rules_stamp);
    c->flags.reported = lz_rd_flag(&r);
    c->flags.ask_button = lz_rd_u8(&r);
    lz_rd_bytes(&r, c->open_id, 16);
    c->open_at = lz_rd_u64(&r);
    c->last_answer = lz_rd_u64(&r);
    c->recent_len = lz_rd_u8(&r);
    if (r.err == LZ_OK && c->recent_len > LZ_RECENT_MAX) r.err = LZ_E_TOO_LONG;
    if (r.err == LZ_OK) {
        uint8_t i;
        for (i = 0; i < c->recent_len; i++) c->recent[i] = lz_rd_u64(&r);
    }
    lz_rd_text(&r, &c->name.len, c->name.b, LZ_MAX_NAME_LEN);
    if (r.err != LZ_OK) {
        lz_wipe(c, sizeof *c);
        return r.err;
    }
    if (lz_is_zero(c->open_id, 16)) c->open_at = 0;
    return LZ_OK;
}

size_t lz_contact_record_encode(const lz_contact_record *c, uint8_t out[LZ_RECORD_MAX_LEN])
{
    lz_wr w;
    uint8_t i, n = c->recent_len > LZ_RECENT_MAX ? LZ_RECENT_MAX : c->recent_len;
    lz_wr_init(&w, out, LZ_RECORD_MAX_LEN);
    lz_wr_bytes(&w, c->secret_key, 32);
    lz_wr_bytes(&w, c->peer_key, 32);
    lz_wr_bytes(&w, c->enrolment, 32);
    lz_wr_text(&w, c->own_relay.len, c->own_relay.b);
    lz_wr_bytes(&w, c->peer_mailbox, 32);
    lz_wr_bytes(&w, c->peer_send_token, 32);
    lz_wr_text(&w, c->peer_relay.len, c->peer_relay.b);
    lz_wr_rules(&w, &c->own_rules);
    lz_wr_stamp(&w, &c->own_rules_stamp);
    lz_wr_rules(&w, &c->peer_rules);
    lz_wr_stamp(&w, &c->peer_rules_stamp);
    lz_wr_u8(&w, c->flags.reported ? 1 : 0);
    lz_wr_u8(&w, c->flags.ask_button);
    lz_wr_bytes(&w, c->open_id, 16);
    lz_wr_u64(&w, lz_is_zero(c->open_id, 16) ? 0 : c->open_at);
    lz_wr_u64(&w, c->last_answer);
    lz_wr_u8(&w, n);
    for (i = 0; i < n; i++) lz_wr_u64(&w, c->recent[i]);
    lz_wr_text(&w, c->name.len, c->name.b);
    return w.err == LZ_OK ? w.pos : 0;
}

/* ---- setup ---- */

int lz_setup_header_decode(lz_setup_header *h, const uint8_t *in, size_t len)
{
    lz_rd r;
    uint8_t version;
    memset(h, 0, sizeof *h);
    lz_rd_init(&r, in, len);
    version = lz_rd_u8(&r);
    if (r.err == LZ_OK && version != LZ_SETUP_VERSION) return LZ_E_VERSION;
    lz_rd_bytes(&r, h->reader, 16);
    lz_rd_text(&r, &h->wifi_name_len, h->wifi_name, LZ_MAX_WIFI_NAME_LEN);
    lz_rd_text(&r, &h->wifi_password_len, h->wifi_password, LZ_MAX_WIFI_PASSWORD_LEN);
    lz_rd_text(&r, &h->relay.len, h->relay.b, LZ_MAX_URL_LEN);
    lz_rd_bytes(&r, h->mailbox, 32);
    lz_rd_bytes(&r, h->enrolment, 32);
    lz_rd_bytes(&r, h->send_token, 32);
    lz_rd_bytes(&r, h->key, 32);
    lz_rd_stamp(&r, &h->settings_stamp);
    lz_rd_settings(&r, &h->settings);
    h->contacts = lz_rd_u8(&r);
    if (r.err != LZ_OK) {
        lz_wipe(h, sizeof *h);
        return r.err;
    }
    h->entries_offset = len - r.len;
    return LZ_OK;
}

int lz_setup_entry_next(const uint8_t *in, size_t len, size_t *offset, uint8_t mailbox[32],
                        lz_contact_record *c)
{
    lz_rd r;
    uint16_t n;
    const uint8_t *rec;
    int err;
    if (*offset > len) return LZ_E_ARG;
    lz_rd_init(&r, in + *offset, len - *offset);
    lz_rd_bytes(&r, mailbox, 32);
    n = lz_rd_u16(&r);
    rec = lz_rd_take(&r, n);
    if (r.err != LZ_OK) return r.err;
    err = lz_contact_record_decode(c, rec, n);
    if (err != LZ_OK) return err;
    *offset = len - r.len;
    return LZ_OK;
}
