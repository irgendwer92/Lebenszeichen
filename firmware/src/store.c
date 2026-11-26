/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The persisted state: one versioned blob, big-endian, with only the used
 * entries of every table, and a SHA-256 over it at the end against damage
 * (not a MAC: whoever can write the flash can also read the keys in it).
 *
 * Not stored, because it follows from the rest: connection state, derived
 * keys, waiting-time timers (they follow from the stored request times and
 * the waiting time, as in the reference satellite).
 */
#include <string.h>

#include "internal.h"

#define MAGIC "LZSB"
#define VERSION 1

/* ---- writing, streamed through a small buffer ---- */

typedef struct {
    lz_write_fn write;
    void *user;
    lz_sha256_state sha;
    uint8_t buf[256];
    size_t used;
    int err;
} sw;

static void sw_flush(sw *w)
{
    if (w->err == LZ_OK && w->used && w->write(w->user, w->buf, w->used) != 0) w->err = LZ_E_FULL;
    lz_wipe(w->buf, w->used);
    w->used = 0;
}

static void sw_bytes(sw *w, const uint8_t *b, size_t n)
{
    if (w->err != LZ_OK) return;
    lz_sha256_update(&w->sha, b, n);
    while (n) {
        size_t take = sizeof w->buf - w->used;
        if (take > n) take = n;
        memcpy(w->buf + w->used, b, take);
        w->used += take;
        b += take;
        n -= take;
        if (w->used == sizeof w->buf) sw_flush(w);
    }
}

static void sw_u8(sw *w, uint8_t v) { sw_bytes(w, &v, 1); }

static void sw_u16(sw *w, uint16_t v)
{
    uint8_t b[2];
    b[0] = (uint8_t)(v >> 8);
    b[1] = (uint8_t)v;
    sw_bytes(w, b, 2);
}

static void sw_u32(sw *w, uint32_t v)
{
    uint8_t b[4];
    lz_put_u32(b, v);
    sw_bytes(w, b, 4);
}

static void sw_u64(sw *w, uint64_t v)
{
    uint8_t b[8];
    lz_put_u64(b, v);
    sw_bytes(w, b, 8);
}

static void sw_text(sw *w, uint8_t len, const uint8_t *b)
{
    sw_u8(w, len);
    sw_bytes(w, b, len);
}

static void sw_opt(sw *w, uint8_t has, const uint8_t v[32])
{
    sw_u8(w, has ? 1 : 0);
    if (has) sw_bytes(w, v, 32);
}

static void sw_stamp(sw *w, const lz_stamp *s)
{
    sw_u64(w, s->at);
    sw_bytes(w, s->id, 16);
}

static void sw_rules(sw *w, const lz_rules *r)
{
    sw_u32(w, r->min_interval_minutes);
    sw_u32(w, r->expiry_minutes);
    sw_u16(w, r->weekly_limit);
}

static void sw_slot(sw *w, const lz_state_slot *s)
{
    sw_u8(w, s->has ? 1 : 0);
    if (!s->has) return;
    sw_text(w, s->reason.len, s->reason.b);
    sw_u64(w, s->until);
    sw_u64(w, s->since);
    sw_u8(w, s->shared ? 1 : 0);
}

static void sw_seen(sw *w, const lz_seen_entry *e, uint8_t n, uint64_t floor)
{
    uint8_t i;
    sw_u64(w, floor);
    sw_u8(w, n);
    for (i = 0; i < n; i++) {
        sw_bytes(w, e[i].id, 16);
        sw_u64(w, e[i].at);
    }
}

static void sw_ledger(sw *w, const lz_ledger *l)
{
    uint8_t i;
    sw_u8(w, l->n);
    sw_u8(w, l->has_last_answer ? 1 : 0);
    sw_u64(w, l->last_answer);
    for (i = 0; i < l->n; i++) {
        sw_bytes(w, l->e[i].id, 16);
        sw_u64(w, l->e[i].at);
        sw_u8(w, l->e[i].answered ? 1 : 0);
        sw_u64(w, l->e[i].answered_at);
        sw_u8(w, l->e[i].reported ? 1 : 0);
    }
}

int lz_save(const lz_ctx *ctx, lz_write_fn write, void *user)
{
    sw w;
    uint8_t hash[32];
    int i, ntransfers = 0;
    if (!ctx || !write || !ctx->installed) return LZ_E_ARG;
    memset(&w, 0, sizeof w);
    w.write = write;
    w.user = user;
    lz_sha256_init(&w.sha);

    sw_bytes(&w, (const uint8_t *)MAGIC, 4);
    sw_u8(&w, VERSION);
    sw_bytes(&w, ctx->reader, 16);
    sw_text(&w, ctx->wifi_name_len, ctx->wifi_name);
    sw_text(&w, ctx->wifi_password_len, ctx->wifi_password);
    sw_u8(&w, ctx->nrelays);
    for (i = 0; i < ctx->nrelays; i++) sw_text(&w, ctx->relays[i].url.len, ctx->relays[i].url.b);

    sw_u8(&w, ctx->channel.relay);
    sw_bytes(&w, ctx->channel.mailbox, 32);
    sw_bytes(&w, ctx->channel.enrolment, 32);
    sw_opt(&w, ctx->channel.has_own_token, ctx->channel.own_token);
    sw_opt(&w, ctx->channel.has_new_token, ctx->channel.new_token);
    sw_bytes(&w, ctx->channel.send_token, 32);
    sw_bytes(&w, ctx->channel.key, 32);
    sw_seen(&w, ctx->channel.seen, ctx->channel.seen_n, ctx->channel.seen_floor);

    sw_u16(&w, ctx->settings.waiting_minutes);
    sw_u32(&w, ctx->settings.preset_minutes);
    sw_u8(&w, ctx->settings.share ? 1 : 0);
    sw_text(&w, ctx->settings.reason.len, ctx->settings.reason.b);
    sw_stamp(&w, &ctx->settings_stamp);
    sw_slot(&w, &ctx->own_state);
    sw_slot(&w, &ctx->handed);
    sw_stamp(&w, &ctx->share_stamp);

    sw_u8(&w, ctx->ncontacts);
    for (i = 0; i < ctx->ncontacts; i++) {
        const lz_contact *c = &ctx->contacts[i];
        sw_bytes(&w, c->own_mailbox, 32);
        sw_u8(&w, c->own_relay);
        sw_bytes(&w, c->enrolment, 32);
        sw_opt(&w, c->has_own_token, c->own_token);
        sw_opt(&w, c->has_new_token, c->new_token);
        sw_text(&w, c->name.len, c->name.b);
        sw_bytes(&w, c->secret_key, 32);
        sw_bytes(&w, c->peer_key, 32);
        sw_bytes(&w, c->peer_mailbox, 32);
        sw_bytes(&w, c->peer_send_token, 32);
        sw_text(&w, c->peer_relay.len, c->peer_relay.b);
        sw_rules(&w, &c->own_rules);
        sw_stamp(&w, &c->own_rules_stamp);
        sw_rules(&w, &c->peer_rules);
        sw_stamp(&w, &c->peer_rules_stamp);
        sw_u8(&w, c->flags.reported ? 1 : 0);
        sw_u8(&w, c->flags.ask_button);
        sw_stamp(&w, &c->flags_stamp);
        sw_u8(&w, c->ended ? 1 : 0);
        sw_ledger(&w, &c->incoming);
        sw_ledger(&w, &c->outgoing);
        sw_seen(&w, c->seen, c->seen_n, c->seen_floor);
        sw_u8(&w, c->answer_shown ? 1 : 0);
        if (c->answer_shown) {
            sw_u8(&w, c->answer_kind);
            sw_text(&w, c->answer_reason.len, c->answer_reason.b);
            sw_u64(&w, c->answer_at);
            sw_u64(&w, c->answer_until);
        }
    }

    for (i = 0; i < LZ_MAX_TRANSFERS; i++) ntransfers += ctx->transfers[i].used ? 1 : 0;
    sw_u8(&w, (uint8_t)ntransfers);
    for (i = 0; i < LZ_MAX_TRANSFERS; i++) {
        const lz_transfer *t = &ctx->transfers[i];
        int p;
        if (!t->used) continue;
        sw_bytes(&w, t->contact, 32);
        sw_bytes(&w, t->transfer, 16);
        sw_u8(&w, t->count);
        sw_u8(&w, t->discarded ? 1 : 0);
        sw_u8(&w, t->have);
        sw_u64(&w, t->first_seen);
        for (p = 0; p < LZ_RECORD_MAX_PARTS; p++) {
            if (t->have & (1u << p)) sw_text(&w, t->lens[p], t->data[p]);
        }
    }

    sw_u8(&w, ctx->nparked);
    for (i = 0; i < ctx->nparked; i++) {
        sw_u64(&w, ctx->parked[i].arrived);
        sw_bytes(&w, ctx->parked[i].plain, LZ_PLAINTEXT_LEN);
    }

    sw_u8(&w, ctx->noutbox);
    for (i = 0; i < ctx->noutbox; i++) {
        const lz_out_entry *o = &ctx->outbox[i];
        sw_u8(&w, o->to_channel ? 1 : 0);
        sw_bytes(&w, o->contact, 32);
        sw_u8(&w, o->kind);
        sw_u64(&w, o->created_at);
        sw_bytes(&w, o->env, LZ_ENVELOPE_LEN);
    }

    lz_sha256_final(&w.sha, hash);
    sw_flush(&w);
    if (w.err == LZ_OK && write(user, hash, 32) != 0) w.err = LZ_E_FULL;
    return w.err;
}

typedef struct {
    uint8_t *buf;
    size_t cap, len;
} bufw;

static int write_buf(void *user, const uint8_t *data, size_t len)
{
    bufw *b = (bufw *)user;
    if (len > b->cap - b->len) return -1;
    memcpy(b->buf + b->len, data, len);
    b->len += len;
    return 0;
}

int lz_save_buf(const lz_ctx *ctx, uint8_t *buf, size_t cap, size_t *len)
{
    bufw b;
    int err;
    if (!buf || !len) return LZ_E_ARG;
    b.buf = buf;
    b.cap = cap;
    b.len = 0;
    err = lz_save(ctx, write_buf, &b);
    *len = err == LZ_OK ? b.len : 0;
    if (err != LZ_OK) lz_wipe(buf, b.len);
    return err;
}

size_t lz_save_max(void)
{
    const size_t seen_entry = 16 + 8, entry = 16 + 8 + 1 + 8 + 1;
    size_t ledger = 1 + 1 + 8 + LZ_MAX_REQUESTS * entry;
    size_t contact = 32 + 1 + 32 + 33 + 33 + (1 + LZ_MAX_NAME_LEN) + 4 * 32 +
                     (1 + LZ_MAX_URL_LEN) + 2 * (10 + 24) + 2 + 24 + 1 + 2 * ledger + 8 + 1 +
                     LZ_MAX_SEEN * seen_entry + 1 + 1 + (1 + LZ_MAX_REASON_LEN) + 16;
    size_t transfer = 32 + 16 + 3 + 8 + LZ_RECORD_MAX_PARTS * (1 + LZ_MAX_PART_LEN);
    size_t n = 4 + 1 + 16 + (1 + LZ_MAX_WIFI_NAME_LEN) + (1 + LZ_MAX_WIFI_PASSWORD_LEN) + 1 +
               LZ_MAX_RELAYS * (1 + LZ_MAX_URL_LEN);
    n += 1 + 32 + 32 + 33 + 33 + 32 + 32 + 8 + 1 + LZ_MAX_CHANNEL_SEEN * seen_entry;
    n += 2 + 4 + 1 + (1 + LZ_MAX_REASON_LEN) + 24;
    n += 2 * (1 + (1 + LZ_MAX_REASON_LEN) + 8 + 8 + 1) + 24;
    n += 1 + LZ_MAX_CONTACTS * contact;
    n += 1 + LZ_MAX_TRANSFERS * transfer;
    n += 1 + LZ_MAX_PARKED * (8 + LZ_PLAINTEXT_LEN);
    n += 1 + LZ_OUTBOX_SIZE * (1 + 32 + 1 + 8 + LZ_ENVELOPE_LEN);
    return n + 32;
}

/* ---- reading ---- */

static void rd_opt(lz_rd *r, uint8_t *has, uint8_t v[32])
{
    *has = lz_rd_flag(r);
    if (*has) lz_rd_bytes(r, v, 32);
}

static void rd_slot(lz_rd *r, lz_state_slot *s)
{
    memset(s, 0, sizeof *s);
    s->has = lz_rd_flag(r);
    if (!s->has) return;
    lz_rd_text(r, &s->reason.len, s->reason.b, LZ_MAX_REASON_LEN);
    s->until = lz_rd_u64(r);
    s->since = lz_rd_u64(r);
    s->shared = lz_rd_flag(r);
}

static void rd_seen(lz_rd *r, lz_seen_entry *e, uint8_t *n, uint8_t cap, uint64_t *floor)
{
    uint8_t i;
    *floor = lz_rd_u64(r);
    *n = lz_rd_u8(r);
    if (r->err == LZ_OK && *n > cap) r->err = LZ_E_LIMIT;
    if (r->err != LZ_OK) {
        *n = 0;
        return;
    }
    for (i = 0; i < *n; i++) {
        lz_rd_bytes(r, e[i].id, 16);
        e[i].at = lz_rd_u64(r);
    }
}

static void rd_ledger(lz_rd *r, lz_ledger *l)
{
    uint8_t i;
    memset(l, 0, sizeof *l);
    l->n = lz_rd_u8(r);
    l->has_last_answer = lz_rd_flag(r);
    l->last_answer = lz_rd_u64(r);
    if (r->err == LZ_OK && l->n > LZ_MAX_REQUESTS) r->err = LZ_E_LIMIT;
    if (r->err != LZ_OK) {
        l->n = 0;
        return;
    }
    for (i = 0; i < l->n; i++) {
        lz_rd_bytes(r, l->e[i].id, 16);
        l->e[i].at = lz_rd_u64(r);
        l->e[i].answered = lz_rd_flag(r);
        l->e[i].answered_at = lz_rd_u64(r);
        l->e[i].reported = lz_rd_flag(r);
        /* Kept sorted by time, as everything else assumes. */
        if (i > 0 && l->e[i].at < l->e[i - 1].at) r->err = LZ_E_VALUE;
    }
}

static int parse(lz_ctx *ctx, const uint8_t *blob, size_t len)
{
    lz_rd r;
    const uint8_t *magic;
    int i;
    uint8_t n;

    lz_rd_init(&r, blob, len);
    magic = lz_rd_take(&r, 4);
    if (!magic || memcmp(magic, MAGIC, 4) != 0) return LZ_E_VALUE;
    if (lz_rd_u8(&r) != VERSION) return r.err != LZ_OK ? r.err : LZ_E_VERSION;
    lz_rd_bytes(&r, ctx->reader, 16);
    lz_rd_text(&r, &ctx->wifi_name_len, ctx->wifi_name, LZ_MAX_WIFI_NAME_LEN);
    lz_rd_text(&r, &ctx->wifi_password_len, ctx->wifi_password, LZ_MAX_WIFI_PASSWORD_LEN);
    ctx->nrelays = lz_rd_u8(&r);
    if (r.err == LZ_OK && (ctx->nrelays == 0 || ctx->nrelays > LZ_MAX_RELAYS)) return LZ_E_LIMIT;
    for (i = 0; i < ctx->nrelays && r.err == LZ_OK; i++) {
        lz_rd_text(&r, &ctx->relays[i].url.len, ctx->relays[i].url.b, LZ_MAX_URL_LEN);
        if (r.err == LZ_OK && !lz_url_ok(&ctx->relays[i].url)) return LZ_E_VALUE;
    }

    ctx->channel.relay = lz_rd_u8(&r);
    if (r.err == LZ_OK && ctx->channel.relay >= ctx->nrelays) return LZ_E_VALUE;
    lz_rd_bytes(&r, ctx->channel.mailbox, 32);
    lz_rd_bytes(&r, ctx->channel.enrolment, 32);
    rd_opt(&r, &ctx->channel.has_own_token, ctx->channel.own_token);
    rd_opt(&r, &ctx->channel.has_new_token, ctx->channel.new_token);
    lz_rd_bytes(&r, ctx->channel.send_token, 32);
    lz_rd_bytes(&r, ctx->channel.key, 32);
    rd_seen(&r, ctx->channel.seen, &ctx->channel.seen_n, LZ_MAX_CHANNEL_SEEN,
            &ctx->channel.seen_floor);

    ctx->settings.waiting_minutes = lz_rd_u16(&r);
    ctx->settings.preset_minutes = lz_rd_u32(&r);
    ctx->settings.share = lz_rd_flag(&r);
    lz_rd_text(&r, &ctx->settings.reason.len, ctx->settings.reason.b, LZ_MAX_REASON_LEN);
    lz_rd_stamp(&r, &ctx->settings_stamp);
    rd_slot(&r, &ctx->own_state);
    rd_slot(&r, &ctx->handed);
    lz_rd_stamp(&r, &ctx->share_stamp);

    n = lz_rd_u8(&r);
    if (r.err == LZ_OK && n > LZ_MAX_CONTACTS) return LZ_E_LIMIT;
    for (i = 0; i < n && r.err == LZ_OK; i++) {
        lz_contact *c = &ctx->contacts[i];
        int j;
        lz_rd_bytes(&r, c->own_mailbox, 32);
        c->own_relay = lz_rd_u8(&r);
        if (r.err == LZ_OK && c->own_relay >= ctx->nrelays) return LZ_E_VALUE;
        for (j = 0; j < i; j++) {
            if (memcmp(ctx->contacts[j].own_mailbox, c->own_mailbox, 32) == 0) return LZ_E_VALUE;
        }
        lz_rd_bytes(&r, c->enrolment, 32);
        rd_opt(&r, &c->has_own_token, c->own_token);
        rd_opt(&r, &c->has_new_token, c->new_token);
        lz_rd_text(&r, &c->name.len, c->name.b, LZ_MAX_NAME_LEN);
        lz_rd_bytes(&r, c->secret_key, 32);
        lz_rd_bytes(&r, c->peer_key, 32);
        lz_rd_bytes(&r, c->peer_mailbox, 32);
        lz_rd_bytes(&r, c->peer_send_token, 32);
        lz_rd_text(&r, &c->peer_relay.len, c->peer_relay.b, LZ_MAX_URL_LEN);
        if (r.err == LZ_OK && !lz_url_ok(&c->peer_relay)) return LZ_E_VALUE;
        lz_rd_rules(&r, &c->own_rules);
        lz_rd_stamp(&r, &c->own_rules_stamp);
        lz_rd_rules(&r, &c->peer_rules);
        lz_rd_stamp(&r, &c->peer_rules_stamp);
        c->flags.reported = lz_rd_flag(&r);
        c->flags.ask_button = lz_rd_u8(&r);
        lz_rd_stamp(&r, &c->flags_stamp);
        c->ended = lz_rd_flag(&r);
        rd_ledger(&r, &c->incoming);
        rd_ledger(&r, &c->outgoing);
        rd_seen(&r, c->seen, &c->seen_n, LZ_MAX_SEEN, &c->seen_floor);
        c->answer_shown = lz_rd_flag(&r);
        if (c->answer_shown) {
            c->answer_kind = lz_rd_u8(&r);
            if (r.err == LZ_OK && c->answer_kind != LZ_KIND_MANUAL &&
                c->answer_kind != LZ_KIND_REPORTED) {
                return LZ_E_VALUE;
            }
            lz_rd_text(&r, &c->answer_reason.len, c->answer_reason.b, LZ_MAX_REASON_LEN);
            c->answer_at = lz_rd_u64(&r);
            c->answer_until = lz_rd_u64(&r);
        }
        ctx->ncontacts = (uint8_t)(i + 1);
    }

    n = lz_rd_u8(&r);
    if (r.err == LZ_OK && n > LZ_MAX_TRANSFERS) return LZ_E_LIMIT;
    for (i = 0; i < n && r.err == LZ_OK; i++) {
        lz_transfer *t = &ctx->transfers[i];
        int p;
        t->used = 1;
        lz_rd_bytes(&r, t->contact, 32);
        lz_rd_bytes(&r, t->transfer, 16);
        t->count = lz_rd_u8(&r);
        t->discarded = lz_rd_flag(&r);
        t->have = lz_rd_u8(&r);
        t->first_seen = lz_rd_u64(&r);
        if (r.err == LZ_OK && (t->have >> LZ_RECORD_MAX_PARTS) != 0) return LZ_E_VALUE;
        for (p = 0; p < LZ_RECORD_MAX_PARTS; p++) {
            if (t->have & (1u << p)) {
                uint8_t l;
                const uint8_t *d;
                l = lz_rd_u8(&r);
                d = lz_rd_take(&r, l);
                if (d && l > LZ_MAX_PART_LEN) return LZ_E_TOO_LONG;
                if (d) {
                    memcpy(t->data[p], d, l);
                    t->lens[p] = l;
                }
            }
        }
    }

    n = lz_rd_u8(&r);
    if (r.err == LZ_OK && n > LZ_MAX_PARKED) return LZ_E_LIMIT;
    for (i = 0; i < n && r.err == LZ_OK; i++) {
        ctx->parked[i].arrived = lz_rd_u64(&r);
        lz_rd_bytes(&r, ctx->parked[i].plain, LZ_PLAINTEXT_LEN);
        ctx->nparked = (uint8_t)(i + 1);
    }

    n = lz_rd_u8(&r);
    if (r.err == LZ_OK && n > LZ_OUTBOX_SIZE) return LZ_E_LIMIT;
    for (i = 0; i < n && r.err == LZ_OK; i++) {
        lz_out_entry *o = &ctx->outbox[i];
        o->to_channel = lz_rd_flag(&r);
        lz_rd_bytes(&r, o->contact, 32);
        o->kind = lz_rd_u8(&r);
        o->created_at = lz_rd_u64(&r);
        lz_rd_bytes(&r, o->env, LZ_ENVELOPE_LEN);
        ctx->noutbox = (uint8_t)(i + 1);
    }
    if (r.err != LZ_OK) return r.err;
    /* Nothing may follow but the hash, which the caller cut off. */
    if (r.len != 0) return LZ_E_TOO_LONG;
    return LZ_OK;
}

int lz_load(lz_ctx *ctx, const uint8_t *blob, size_t len)
{
    uint8_t hash[32];
    lz_platform pf;
    int err;
    if (!ctx || !blob) return LZ_E_ARG;
    pf = ctx->pf;
    lz_wipe(ctx, sizeof *ctx);
    ctx->pf = pf;
    ctx->led_sent = 0xFF;
    if (len < 32 + 5) return LZ_E_TRUNCATED;
    lz_sha256(hash, blob, len - 32);
    if (!lz_ct_equal(hash, blob + len - 32, 32)) return LZ_E_VALUE;
    err = parse(ctx, blob, len - 32);
    if (err != LZ_OK) {
        lz_wipe(ctx, sizeof *ctx);
        ctx->pf = pf;
        ctx->led_sent = 0xFF;
        return err;
    }
    ctx->installed = 1;
    ctx->view_dirty = 1;
    return LZ_OK;
}
