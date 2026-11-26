/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The relay side of the satellite as data: which WebSockets to open, the
 * frames to send on them, the HTTP requests to make, and what their
 * answers mean. Mirrors RefSatellite in crates/client/src/tests/reference.rs.
 */
#include <string.h>

#include "internal.h"

enum { HTTP_HEALTH = 1, HTTP_TOKEN, HTTP_DELIVER };

#define HTTP_TIMEOUT 60u
#define TOKEN_RETRY 30u
#define DELIVER_RETRY_MIN 10u
#define DELIVER_RETRY_MAX 300u
#define CONNECT_BACKOFF_MAX 300u
#define HEALTH_EVERY 3600u
#define HEALTH_WHILE_SKEWED 60u

/* ---- mailboxes: 0 is the channel, 1 + i contact i ---- */

typedef struct {
    uint8_t relay;
    uint8_t *mailbox;
    uint8_t *enrolment;
    uint8_t *has_own;
    uint8_t *own;
    uint8_t *has_new;
    uint8_t *fresh;
    int active;
} mb_ref;

static int mb_count(const lz_ctx *ctx) { return 1 + ctx->ncontacts; }

static void mb_get(lz_ctx *ctx, int k, mb_ref *m)
{
    if (k == 0) {
        lz_channel *ch = &ctx->channel;
        m->relay = ch->relay;
        m->mailbox = ch->mailbox;
        m->enrolment = ch->enrolment;
        m->has_own = &ch->has_own_token;
        m->own = ch->own_token;
        m->has_new = &ch->has_new_token;
        m->fresh = ch->new_token;
        m->active = 1;
    } else {
        lz_contact *c = &ctx->contacts[k - 1];
        m->relay = c->own_relay;
        m->mailbox = c->own_mailbox;
        m->enrolment = c->enrolment;
        m->has_own = &c->has_own_token;
        m->own = c->own_token;
        m->has_new = &c->has_new_token;
        m->fresh = c->new_token;
        m->active = !c->ended;
    }
}

/* Ready to subscribe: with the own token, or the replacement failed for
 * good and the enrolment token is used to learn why. */
static int mb_ready(lz_ctx *ctx, int k)
{
    mb_ref m;
    mb_get(ctx, k, &m);
    return *m.has_own || ctx->rt[k].token_job == 2;
}

static int mb_find(lz_ctx *ctx, uint8_t relay, const uint8_t mailbox[32])
{
    int k;
    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        mb_get(ctx, k, &m);
        if (m.relay == relay && memcmp(m.mailbox, mailbox, 32) == 0) return k;
    }
    return -1;
}

static int relay_needed(lz_ctx *ctx, int r)
{
    int k;
    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        mb_get(ctx, k, &m);
        if (m.active && m.relay == r) return 1;
    }
    return 0;
}

static void resubscribe_all(lz_ctx *ctx)
{
    int k;
    for (k = 0; k < mb_count(ctx); k++) {
        if (ctx->rt[k].sub != 0) ctx->rt[k].need_sub = 1;
    }
}

/* ---- setup ---- */

int lz_init(lz_ctx *ctx, const lz_platform *pf)
{
    if (!ctx || !pf || !pf->now || !pf->random) return LZ_E_ARG;
    lz_wipe(ctx, sizeof *ctx);
    ctx->pf = *pf;
    ctx->led_sent = 0xFF;
    if (lz_crypto_init() != 0) return LZ_E_CRYPTO;
    return LZ_OK;
}

int lz_install(lz_ctx *ctx, const uint8_t *setup, size_t len)
{
    lz_setup_header h;
    lz_contact_record rec;
    uint8_t mailboxes[LZ_MAX_CONTACTS][32];
    lz_url relays[LZ_MAX_RELAYS];
    size_t off;
    int nrelays = 1, i, j, err;
    lz_platform pf;

    if (!ctx || !setup) return LZ_E_ARG;
    if ((err = lz_setup_header_decode(&h, setup, len)) != LZ_OK) return err;
    if (!lz_url_ok(&h.relay)) return LZ_E_VALUE;
    if (h.contacts > LZ_MAX_CONTACTS) return LZ_E_LIMIT;
    relays[0] = h.relay;

    /* First pass: everything must be readable and within the limits. */
    off = h.entries_offset;
    for (i = 0; i < h.contacts; i++) {
        int found = 0;
        if ((err = lz_setup_entry_next(setup, len, &off, mailboxes[i], &rec)) != LZ_OK) goto fail;
        for (j = 0; j < i; j++) {
            if (memcmp(mailboxes[j], mailboxes[i], 32) == 0) {
                err = LZ_E_VALUE;
                goto fail;
            }
        }
        if (memcmp(mailboxes[i], h.mailbox, 32) == 0 || !lz_url_ok(&rec.own_relay) ||
            !lz_url_ok(&rec.peer_relay)) {
            err = LZ_E_VALUE;
            goto fail;
        }
        for (j = 0; j < nrelays; j++) {
            if (relays[j].len == rec.own_relay.len &&
                memcmp(relays[j].b, rec.own_relay.b, rec.own_relay.len) == 0) {
                found = 1;
            }
        }
        if (!found) {
            if (nrelays == LZ_MAX_RELAYS) {
                err = LZ_E_LIMIT;
                goto fail;
            }
            relays[nrelays++] = rec.own_relay;
        }
    }

    /* Second pass: build the state. */
    pf = ctx->pf;
    lz_wipe(ctx, sizeof *ctx);
    ctx->pf = pf;
    ctx->led_sent = 0xFF;
    memcpy(ctx->reader, h.reader, 16);
    ctx->wifi_name_len = h.wifi_name_len;
    memcpy(ctx->wifi_name, h.wifi_name, h.wifi_name_len);
    ctx->wifi_password_len = h.wifi_password_len;
    memcpy(ctx->wifi_password, h.wifi_password, h.wifi_password_len);
    ctx->nrelays = 1;
    ctx->relays[0].url = h.relay;
    ctx->channel.relay = 0;
    memcpy(ctx->channel.mailbox, h.mailbox, 32);
    memcpy(ctx->channel.enrolment, h.enrolment, 32);
    memcpy(ctx->channel.send_token, h.send_token, 32);
    memcpy(ctx->channel.key, h.key, 32);
    ctx->settings = h.settings;
    ctx->settings_stamp = h.settings_stamp;
    off = h.entries_offset;
    for (i = 0; i < h.contacts; i++) {
        uint8_t mailbox[32];
        if ((err = lz_setup_entry_next(setup, len, &off, mailbox, &rec)) != LZ_OK ||
            (err = lz_sat_add_contact(ctx, mailbox, &rec)) != LZ_OK) {
            pf = ctx->pf;
            lz_wipe(ctx, sizeof *ctx);
            ctx->pf = pf;
            ctx->led_sent = 0xFF;
            goto fail;
        }
    }
    ctx->installed = 1;
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    err = LZ_OK;
fail:
    lz_wipe(&h, sizeof h);
    lz_wipe(&rec, sizeof rec);
    return err;
}

void lz_factory_reset(lz_ctx *ctx)
{
    lz_sat_wipe(ctx);
    /* A reset is not a removal on the phone. */
    ctx->removed = 0;
    ctx->ev_count = 0;
}

/* ---- inputs ---- */

static void clock_check(lz_ctx *ctx, uint64_t date)
{
    uint64_t skew;
    if (date == 0) return;
    skew = date > ctx->now ? date - ctx->now : ctx->now - date;
    if (skew > LZ_MAX_CLOCK_SKEW) {
        if (!ctx->clock_bad) {
            ctx->clock_bad = 1;
            lz_event_push(ctx, LZ_EV_CLOCK_WARNING, -1);
            lz_event_last(ctx)->at = date;
            ctx->view_dirty = 1;
        }
        ctx->health_due = lz_sat_add(ctx->now, HEALTH_WHILE_SKEWED);
        return;
    }
    ctx->clock_checked = 1;
    if (ctx->clock_bad) {
        ctx->clock_bad = 0;
        lz_event_push(ctx, LZ_EV_CLOCK_OK, -1);
        ctx->view_dirty = 1;
        /* Messages left unacknowledged while the clock was off come
         * again. */
        resubscribe_all(ctx);
    }
}

static void relay_down(lz_ctx *ctx, int r)
{
    lz_relay *rl = &ctx->relays[r];
    int k;
    uint8_t jitter = 0;
    rl->conn = 0;
    rl->backoff = rl->backoff ? rl->backoff * 2 : 1;
    if (rl->backoff > CONNECT_BACKOFF_MAX) rl->backoff = CONNECT_BACKOFF_MAX;
    if (lz_rand(ctx, &jitter, 1) != LZ_OK) jitter = 0;
    rl->next_connect = lz_sat_add(ctx->now, rl->backoff + jitter % (rl->backoff / 4 + 1));
    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        mb_get(ctx, k, &m);
        if (m.relay == r) {
            ctx->rt[k].sub = 0;
            ctx->rt[k].sub_own = 0;
            ctx->rt[k].need_sub = 0;
            ctx->rt[k].need_ack = 0;
        }
    }
}

static void relay_up(lz_ctx *ctx, int r)
{
    lz_relay *rl = &ctx->relays[r];
    int k;
    rl->conn = 2;
    rl->backoff = 0;
    rl->health = 0;
    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        mb_get(ctx, k, &m);
        if (m.relay == r) {
            ctx->rt[k].sub = 0;
            ctx->rt[k].need_sub = 1;
            ctx->rt[k].need_ack = 0;
            ctx->rt[k].lost = 0;
            if (ctx->rt[k].token_job == 2) ctx->rt[k].token_job = 0;
        }
    }
}

static void handle_frame(lz_ctx *ctx, uint8_t r, const uint8_t *text, size_t len)
{
    lz_frame f;
    lz_mbox_rt *rt;
    int k;
    if (lz_frame_parse(&f, (const char *)text, len) != LZ_OK) return;
    if (!f.has_mailbox) return;
    k = mb_find(ctx, r, f.mailbox);
    if (k < 0) goto out;
    rt = &ctx->rt[k];
    if (rt->sub == 0) goto out; /* nothing subscribed on this connection */
    switch (f.op) {
    case LZ_FRAME_SUBSCRIBED:
        rt->sub = 2;
        break;
    case LZ_FRAME_MESSAGE:
        /* With the clock off, leave it on the relay: no ack. */
        if (ctx->clock_bad) break;
        if (k == 0) {
            lz_sat_receive_channel(ctx, f.data, LZ_ENVELOPE_LEN);
        } else {
            lz_sat_receive_contact(ctx, k - 1, f.data, LZ_ENVELOPE_LEN);
        }
        if (f.seq > rt->ack_seq) rt->ack_seq = f.seq;
        rt->need_ack = 1;
        break;
    case LZ_FRAME_ERROR:
        if (k == 0 && (strcmp(f.code, "unknown_reader") == 0 ||
                       strcmp(f.code, "unknown_mailbox") == 0)) {
            /* The channel is gone or no longer knows the satellite: it was
             * removed on a full device. Erase everything. */
            lz_sat_wipe(ctx);
            break;
        }
        if (k > 0 && strcmp(f.code, "unknown_mailbox") == 0) {
            /* The contact was ended or cleaned up on a full device. */
            lz_event *e;
            lz_event_push(ctx, LZ_EV_CONTACT_REMOVED, k - 1);
            e = lz_event_last(ctx);
            e->contact = 0xFF;
            lz_sat_remove_contact(ctx, k - 1);
            break;
        }
        if (strcmp(f.code, "unauthorized") == 0) {
            mb_ref m;
            mb_get(ctx, k, &m);
            if (!rt->sub_own && *m.has_own) {
                /* Subscribed with the enrolment token, replaced meanwhile. */
                rt->sub = 1;
                rt->need_sub = 1;
                break;
            }
            rt->sub = 0;
            rt->lost = 1;
            lz_event_push(ctx, LZ_EV_ACCESS_LOST, k == 0 ? -1 : k - 1);
            ctx->view_dirty = 1;
            break;
        }
        /* Other errors end the subscription until the next connection. */
        rt->sub = 0;
        break;
    default:
        break;
    }
out:
    lz_wipe(&f, sizeof f);
}

static int out_find(lz_ctx *ctx, const uint8_t head[32])
{
    int i;
    for (i = 0; i < ctx->noutbox; i++) {
        if (memcmp(ctx->outbox[i].env, head, 32) == 0) return i;
    }
    return -1;
}

static void out_remove(lz_ctx *ctx, int i)
{
    memmove(&ctx->outbox[i], &ctx->outbox[i + 1], (size_t)(ctx->noutbox - i - 1) * sizeof ctx->outbox[0]);
    ctx->noutbox--;
    memset(&ctx->outbox[ctx->noutbox], 0, sizeof ctx->outbox[0]);
    ctx->dirty = 1;
}

static void http_done(lz_ctx *ctx, int status, uint64_t date, const uint8_t *body, size_t len)
{
    int failed = status <= 0;
    ctx->http_busy = 0;
    if (!failed) clock_check(ctx, date);
    switch (ctx->http_kind) {
    case HTTP_HEALTH:
        if (ctx->http_relay < ctx->nrelays) {
            lz_relay *rl = &ctx->relays[ctx->http_relay];
            /* Done until the next connection, also if it failed. */
            rl->health = 2;
            if (!failed && status == 200) {
                rl->reader_tokens =
                    (uint8_t)lz_health_has_reader_tokens((const char *)body, len);
                if (!rl->reader_tokens) {
                    lz_event_push(ctx, LZ_EV_NO_READER_TOKENS, -1);
                    lz_event_last(ctx)->relay = ctx->http_relay;
                }
            }
        }
        break;
    case HTTP_TOKEN: {
        int k = mb_find(ctx, ctx->http_relay, ctx->http_target);
        mb_ref m;
        lz_mbox_rt *rt;
        if (k < 0) break;
        mb_get(ctx, k, &m);
        rt = &ctx->rt[k];
        rt->token_job = 0;
        if (!failed && status >= 200 && status < 300) {
            memcpy(m.own, m.fresh, 32);
            *m.has_own = 1;
            lz_wipe(m.fresh, 32);
            *m.has_new = 0;
            rt->token_try = 0;
            rt->need_sub = 1; /* again, with the own token */
            ctx->dirty = 1;
        } else if (!failed && status == 401 && rt->token_try == 0) {
            /* The relay may have taken the new token with a lost answer. */
            rt->token_try = 1;
        } else if (!failed && (status == 400 || status == 401 || status == 403 || status == 404)) {
            /* For good: subscribe with the enrolment token, whose answer
             * says whether the satellite was removed. */
            rt->token_job = 2;
            rt->token_try = 0;
            rt->need_sub = 1;
        } else {
            rt->token_retry_at = lz_sat_add(ctx->now, TOKEN_RETRY);
        }
        break;
    }
    case HTTP_DELIVER: {
        int i = out_find(ctx, ctx->http_target);
        if (i < 0) break;
        if (!failed && ((status >= 200 && status < 300) || status == 400)) {
            /* Delivered, or refused for good (400: never deliverable). */
            out_remove(ctx, i);
        } else {
            lz_out_entry *o = &ctx->outbox[i];
            o->backoff = o->backoff ? o->backoff * 2 : DELIVER_RETRY_MIN;
            if (o->backoff > DELIVER_RETRY_MAX) o->backoff = DELIVER_RETRY_MAX;
            o->next_try = lz_sat_add(ctx->now, o->backoff);
        }
        break;
    }
    default:
        break;
    }
}

static int time_ok(const lz_ctx *ctx) { return ctx->now >= LZ_MIN_TIME; }

int lz_feed(lz_ctx *ctx, const lz_input *in)
{
    int r;
    if (!ctx || !in) return LZ_E_ARG;
    if (!ctx->installed) return LZ_E_STATE;
    ctx->now = ctx->pf.now(ctx->pf.user);
    r = in->relay;
    switch (in->type) {
    case LZ_IN_TICK:
        if (!time_ok(ctx)) break;
        if (ctx->http_busy && ctx->now >= ctx->http_deadline) http_done(ctx, 0, 0, NULL, 0);
        if (ctx->now >= ctx->health_due && ctx->nrelays > 0) {
            ctx->relays[ctx->channel.relay].health = 0;
            ctx->health_due = lz_sat_add(ctx->now, ctx->clock_bad || !ctx->clock_checked
                                                       ? HEALTH_WHILE_SKEWED
                                                       : HEALTH_EVERY);
        }
        if (ctx->clock_bad) {
            lz_sat_expire(ctx);
        } else {
            lz_sat_tick(ctx);
        }
        break;
    case LZ_IN_ONLINE: {
        int i;
        ctx->online = 1;
        for (i = 0; i < ctx->nrelays; i++) {
            ctx->relays[i].next_connect = 0;
            ctx->relays[i].backoff = 0;
        }
        break;
    }
    case LZ_IN_OFFLINE: {
        int i;
        ctx->online = 0;
        for (i = 0; i < ctx->nrelays; i++) {
            if (ctx->relays[i].conn) relay_down(ctx, i);
            ctx->relays[i].backoff = 0;
            ctx->relays[i].next_connect = 0;
        }
        break;
    }
    case LZ_IN_CONNECTED:
        if (r >= ctx->nrelays) return LZ_E_ARG;
        relay_up(ctx, r);
        break;
    case LZ_IN_DISCONNECTED:
        if (r >= ctx->nrelays) return LZ_E_ARG;
        relay_down(ctx, r);
        break;
    case LZ_IN_FRAME:
        if (r >= ctx->nrelays || (!in->data && in->len)) return LZ_E_ARG;
        if (ctx->relays[r].conn != 2 || !time_ok(ctx)) break;
        handle_frame(ctx, (uint8_t)r, in->data, in->len);
        break;
    case LZ_IN_HTTP_RESPONSE:
    case LZ_IN_HTTP_FAILED:
        if (!ctx->http_busy || in->handle != ctx->http_handle) break; /* stale */
        if (in->type == LZ_IN_HTTP_FAILED) {
            http_done(ctx, 0, 0, NULL, 0);
        } else {
            http_done(ctx, in->status > 0 ? in->status : 0, in->date, in->data,
                      in->data ? in->len : 0);
        }
        break;
    case LZ_IN_PRESS:
    case LZ_IN_LONG_PRESS:
    case LZ_IN_ASK:
        if (!time_ok(ctx) || ctx->clock_bad) {
            /* Nothing is sent with a clock that is off. */
            lz_event_push(ctx, LZ_EV_CLOCK_WARNING, -1);
            break;
        }
        lz_sat_expire(ctx);
        if (in->type == LZ_IN_PRESS) {
            lz_sat_press(ctx);
        } else if (in->type == LZ_IN_LONG_PRESS) {
            lz_sat_long_press(ctx);
        } else {
            lz_sat_ask(ctx, in->button);
        }
        break;
    default:
        return LZ_E_ARG;
    }
    if (ctx->installed && time_ok(ctx)) lz_sat_track_open(ctx);
    return LZ_OK;
}

/* ---- outputs ---- */

static int view_led(const lz_ctx *ctx)
{
    int i, k, led = LZ_LED_OFF;
    uint64_t newest = 0;
    if (!ctx->installed) return LZ_LED_OFF;
    if (ctx->clock_bad) return LZ_LED_WARNING;
    for (k = 0; k < mb_count(ctx); k++) {
        if (ctx->rt[k].lost) return LZ_LED_WARNING;
    }
    for (i = 0; i < ctx->ncontacts; i++) {
        const lz_contact *c = &ctx->contacts[i];
        if (!c->ended && lz_ledger_open(&c->incoming, &c->own_rules, ctx->now) >= 0) {
            return LZ_LED_REQUEST;
        }
    }
    for (i = 0; i < ctx->ncontacts; i++) {
        const lz_contact *c = &ctx->contacts[i];
        if (c->answer_shown && ctx->now < c->answer_until && c->answer_at >= newest) {
            newest = c->answer_at;
            led = c->answer_kind == LZ_KIND_REPORTED ? LZ_LED_ANSWER_REPORTED : LZ_LED_ANSWER_MANUAL;
        }
    }
    return led;
}

static size_t put_str(char *out, size_t cap, size_t pos, const char *s, size_t n)
{
    if (pos == (size_t)-1 || n >= cap - pos) return (size_t)-1;
    memcpy(out + pos, s, n);
    out[pos + n] = '\0';
    return pos + n;
}

static size_t put_b64(char *out, size_t cap, size_t pos, const uint8_t *b, size_t n)
{
    char tmp[64];
    size_t len = lz_b64url_encode(tmp, sizeof tmp, b, n);
    pos = put_str(out, cap, pos, tmp, len);
    lz_wipe(tmp, sizeof tmp);
    return pos;
}

/* relay address + path into ctx->url. */
static int build_url(lz_ctx *ctx, const lz_url *relay, const char *a, const uint8_t *id1,
                     size_t n1, const char *b, const uint8_t *id2, size_t n2, const char *c)
{
    size_t p = 0;
    if (!lz_url_ok(relay)) return 0;
    p = put_str(ctx->url, sizeof ctx->url, p, (const char *)relay->b, relay->len);
    p = put_str(ctx->url, sizeof ctx->url, p, a, strlen(a));
    if (id1) p = put_b64(ctx->url, sizeof ctx->url, p, id1, n1);
    if (b) p = put_str(ctx->url, sizeof ctx->url, p, b, strlen(b));
    if (id2) p = put_b64(ctx->url, sizeof ctx->url, p, id2, n2);
    if (c) p = put_str(ctx->url, sizeof ctx->url, p, c, strlen(c));
    return p != (size_t)-1;
}

static int build_auth(lz_ctx *ctx, const uint8_t token[32])
{
    size_t p = put_str(ctx->auth, sizeof ctx->auth, 0, "Bearer ", 7);
    p = put_b64(ctx->auth, sizeof ctx->auth, p, token, 32);
    return p != (size_t)-1;
}

static void http_out(lz_ctx *ctx, lz_output *out, int kind, const char *method)
{
    ctx->http_handle++;
    if (ctx->http_handle == 0) ctx->http_handle = 1;
    ctx->http_busy = 1;
    ctx->http_kind = (uint8_t)kind;
    ctx->http_deadline = lz_sat_add(ctx->now, HTTP_TIMEOUT);
    out->type = LZ_OUT_HTTP;
    out->handle = ctx->http_handle;
    out->method = method;
    out->url = ctx->url;
}

static int poll_http(lz_ctx *ctx, lz_output *out)
{
    int r, k, i;
    if (ctx->http_busy || !ctx->online || !time_ok(ctx)) return 0;

    /* The relay's health and clock, once per connection. */
    for (r = 0; r < ctx->nrelays; r++) {
        lz_relay *rl = &ctx->relays[r];
        int recheck = (ctx->clock_bad || !ctx->clock_checked) && r == ctx->channel.relay;
        if (rl->health == 0 && (rl->conn == 2 || recheck)) {
            if (!build_url(ctx, &rl->url, "/v1/health", NULL, 0, NULL, NULL, 0, NULL)) {
                rl->health = 2;
                continue;
            }
            rl->health = 1;
            ctx->http_relay = (uint8_t)r;
            http_out(ctx, out, HTTP_HEALTH, "GET");
            return 1;
        }
    }

    /* Own tokens in place of the enrolment tokens. */
    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        lz_mbox_rt *rt = &ctx->rt[k];
        size_t n;
        mb_get(ctx, k, &m);
        if (!m.active || *m.has_own || rt->token_job != 0 || ctx->now < rt->token_retry_at) continue;
        if (!*m.has_new) {
            /* Stored before it is sent: persist first. */
            if (lz_rand(ctx, m.fresh, 32) != LZ_OK) return 0;
            *m.has_new = 1;
            out->type = LZ_OUT_PERSIST;
            return 1;
        }
        if (!build_url(ctx, &ctx->relays[m.relay].url, "/v1/mailboxes/", m.mailbox, 32,
                       "/readers/", ctx->reader, 16, NULL) ||
            !build_auth(ctx, rt->token_try ? m.fresh : m.enrolment)) {
            rt->token_job = 2;
            continue;
        }
        n = lz_body_token_hash(ctx->text, sizeof ctx->text, m.fresh);
        rt->token_job = 1;
        ctx->http_relay = m.relay;
        memcpy(ctx->http_target, m.mailbox, 32);
        http_out(ctx, out, HTTP_TOKEN, "PUT");
        out->authorization = ctx->auth;
        out->content_type = "application/json";
        out->body = (const uint8_t *)ctx->text;
        out->body_len = n;
        return 1;
    }

    /* The outbox, in order per target, once the clock agrees with a
     * relay's. */
    if (ctx->clock_bad || !ctx->clock_checked) return 0;
    for (i = 0; i < ctx->noutbox; i++) {
        lz_out_entry *o = &ctx->outbox[i];
        const lz_url *relay;
        const uint8_t *mailbox, *token;
        int j, blocked = 0;
        if (ctx->now < o->next_try) {
            continue;
        }
        for (j = 0; j < i; j++) {
            const lz_out_entry *p = &ctx->outbox[j];
            if (p->to_channel == o->to_channel &&
                (o->to_channel || memcmp(p->contact, o->contact, 32) == 0)) {
                blocked = 1;
            }
        }
        if (blocked) continue;
        if (o->to_channel) {
            relay = &ctx->relays[ctx->channel.relay].url;
            mailbox = ctx->channel.mailbox;
            token = ctx->channel.send_token;
        } else {
            int ci = lz_sat_contact_index(ctx, o->contact);
            if (ci < 0) {
                /* The contact is gone: nothing to deliver to. */
                out_remove(ctx, i);
                i--;
                continue;
            }
            relay = &ctx->contacts[ci].peer_relay;
            mailbox = ctx->contacts[ci].peer_mailbox;
            token = ctx->contacts[ci].peer_send_token;
        }
        if (!build_url(ctx, relay, "/v1/mailboxes/", mailbox, 32, "/messages", NULL, 0, NULL) ||
            !build_auth(ctx, token)) {
            out_remove(ctx, i);
            i--;
            continue;
        }
        memcpy(ctx->http_target, o->env, 32);
        http_out(ctx, out, HTTP_DELIVER, "POST");
        out->authorization = ctx->auth;
        out->content_type = "application/octet-stream";
        out->body = o->env;
        out->body_len = LZ_ENVELOPE_LEN;
        return 1;
    }
    return 0;
}

int lz_poll(lz_ctx *ctx, lz_output *out)
{
    int r, k, led;
    if (!ctx || !out) return 0;
    memset(out, 0, sizeof *out);
    if (ctx->erase) {
        ctx->erase = 0;
        out->type = LZ_OUT_ERASE;
        return 1;
    }
    if (ctx->dirty && ctx->installed) {
        ctx->dirty = 0;
        out->type = LZ_OUT_PERSIST;
        return 1;
    }
    if (ctx->ev_count) {
        out->type = LZ_OUT_EVENT;
        out->event = ctx->events[ctx->ev_head];
        lz_wipe(&ctx->events[ctx->ev_head], sizeof ctx->events[0]);
        ctx->ev_head = (uint8_t)((ctx->ev_head + 1) % LZ_MAX_EVENTS);
        ctx->ev_count--;
        return 1;
    }
    if (ctx->view_dirty) {
        ctx->view_dirty = 0;
        out->type = LZ_OUT_VIEW;
        return 1;
    }
    led = view_led(ctx);
    if (led != ctx->led_sent) {
        ctx->led_sent = (uint8_t)led;
        ctx->led = (uint8_t)led;
        out->type = LZ_OUT_LED;
        out->led = led;
        return 1;
    }
    if (!ctx->installed) return 0;

    for (r = 0; r < ctx->nrelays; r++) {
        lz_relay *rl = &ctx->relays[r];
        int needed = relay_needed(ctx, r);
        if (rl->conn != 0 && (!needed || !ctx->online)) {
            relay_down(ctx, r);
            rl->backoff = 0;
            rl->next_connect = 0;
            out->type = LZ_OUT_DISCONNECT;
            out->relay = (uint8_t)r;
            return 1;
        }
        if (rl->conn == 0 && needed && ctx->online && time_ok(ctx) && ctx->now >= rl->next_connect) {
            size_t n = lz_stream_url(ctx->url, sizeof ctx->url, &rl->url);
            if (n == 0) continue;
            rl->conn = 1;
            rl->connect_started = ctx->now;
            out->type = LZ_OUT_CONNECT;
            out->relay = (uint8_t)r;
            out->text = ctx->url;
            out->len = n;
            return 1;
        }
    }

    for (k = 0; k < mb_count(ctx); k++) {
        mb_ref m;
        lz_mbox_rt *rt = &ctx->rt[k];
        size_t n;
        mb_get(ctx, k, &m);
        if (ctx->relays[m.relay].conn != 2) continue;
        if (rt->need_sub && m.active && !rt->lost && time_ok(ctx) && mb_ready(ctx, k)) {
            n = lz_frame_subscribe(ctx->text, sizeof ctx->text, m.mailbox,
                                   *m.has_own ? m.own : m.enrolment, ctx->reader);
            rt->need_sub = 0;
            if (n == 0) continue;
            rt->sub = 1;
            rt->sub_own = *m.has_own;
            out->type = LZ_OUT_SEND;
            out->relay = m.relay;
            out->text = ctx->text;
            out->len = n;
            return 1;
        }
        if (rt->need_ack && rt->sub != 0) {
            n = lz_frame_ack(ctx->text, sizeof ctx->text, m.mailbox, rt->ack_seq);
            rt->need_ack = 0;
            if (n == 0) continue;
            out->type = LZ_OUT_SEND;
            out->relay = m.relay;
            out->text = ctx->text;
            out->len = n;
            return 1;
        }
    }

    return poll_http(ctx, out);
}

/* ---- helpers ---- */

static int feed(lz_ctx *ctx, int type, uint8_t relay, uint8_t button)
{
    lz_input in;
    memset(&in, 0, sizeof in);
    in.type = type;
    in.relay = relay;
    in.button = button;
    return lz_feed(ctx, &in);
}

int lz_tick(lz_ctx *ctx) { return feed(ctx, LZ_IN_TICK, 0, 0); }

int lz_online(lz_ctx *ctx, int up) { return feed(ctx, up ? LZ_IN_ONLINE : LZ_IN_OFFLINE, 0, 0); }

int lz_connected(lz_ctx *ctx, uint8_t relay, int up)
{
    return feed(ctx, up ? LZ_IN_CONNECTED : LZ_IN_DISCONNECTED, relay, 0);
}

int lz_received(lz_ctx *ctx, uint8_t relay, const char *text, size_t len)
{
    lz_input in;
    memset(&in, 0, sizeof in);
    in.type = LZ_IN_FRAME;
    in.relay = relay;
    in.data = (const uint8_t *)text;
    in.len = len;
    return lz_feed(ctx, &in);
}

int lz_http_response(lz_ctx *ctx, uint32_t handle, int status, uint64_t date,
                     const uint8_t *body, size_t len)
{
    lz_input in;
    memset(&in, 0, sizeof in);
    in.type = LZ_IN_HTTP_RESPONSE;
    in.handle = handle;
    in.status = status;
    in.date = date;
    in.data = body;
    in.len = len;
    return lz_feed(ctx, &in);
}

int lz_http_failed(lz_ctx *ctx, uint32_t handle)
{
    lz_input in;
    memset(&in, 0, sizeof in);
    in.type = LZ_IN_HTTP_FAILED;
    in.handle = handle;
    return lz_feed(ctx, &in);
}

int lz_press(lz_ctx *ctx) { return feed(ctx, LZ_IN_PRESS, 0, 0); }

int lz_long_press(lz_ctx *ctx) { return feed(ctx, LZ_IN_LONG_PRESS, 0, 0); }

int lz_ask(lz_ctx *ctx, uint8_t button) { return feed(ctx, LZ_IN_ASK, 0, button); }

int lz_set_state(lz_ctx *ctx, const uint8_t *reason, size_t reason_len, uint32_t minutes,
                 int share)
{
    lz_reason r;
    int err;
    if (!ctx || !ctx->installed) return LZ_E_STATE;
    if ((!reason && reason_len) || reason_len > LZ_MAX_REASON_LEN) return LZ_E_ARG;
    ctx->now = ctx->pf.now(ctx->pf.user);
    if (!time_ok(ctx) || ctx->clock_bad) return LZ_E_STATE;
    r.len = (uint8_t)reason_len;
    if (reason_len) memcpy(r.b, reason, reason_len);
    err = lz_sat_set_state(ctx, &r, minutes, share);
    lz_sat_track_open(ctx);
    return err;
}

int lz_clear_state(lz_ctx *ctx)
{
    int err;
    if (!ctx || !ctx->installed) return LZ_E_STATE;
    ctx->now = ctx->pf.now(ctx->pf.user);
    if (!time_ok(ctx) || ctx->clock_bad) return LZ_E_STATE;
    err = lz_sat_clear_state(ctx);
    lz_sat_track_open(ctx);
    return err;
}

/* ---- the view ---- */

void lz_view(const lz_ctx *ctx, lz_view_t *v)
{
    lz_state_slot state;
    int i;
    memset(v, 0, sizeof *v);
    v->installed = ctx->installed;
    v->removed = ctx->removed;
    v->clock_warning = ctx->clock_bad;
    v->led = view_led(ctx);
    for (i = 0; i < ctx->ncontacts; i++) {
        const lz_contact *c = &ctx->contacts[i];
        if (!c->ended && lz_ledger_open(&c->incoming, &c->own_rules, ctx->now) >= 0) {
            v->open[v->n_open++] = (uint8_t)i;
        }
        if (c->answer_shown && ctx->now < c->answer_until) {
            v->answers[v->n_answers].contact = (uint8_t)i;
            v->answers[v->n_answers].kind = c->answer_kind;
            v->answers[v->n_answers].at = c->answer_at;
            v->answers[v->n_answers].reason = c->answer_reason;
            v->n_answers++;
        }
    }
    i = lz_sat_current_state(ctx, ctx->now, &state);
    if (i) {
        v->state_set = 1;
        v->state_handed = i == 2;
        v->state_reason = state.reason;
        v->state_until = state.until;
    }
}

uint8_t lz_contact_count(const lz_ctx *ctx) { return ctx->ncontacts; }

const lz_cname *lz_contact_name(const lz_ctx *ctx, uint8_t contact)
{
    return contact < ctx->ncontacts ? &ctx->contacts[contact].name : NULL;
}

uint64_t lz_next_request_at(const lz_ctx *ctx, uint8_t contact, uint64_t now)
{
    if (contact >= ctx->ncontacts) return UINT64_MAX;
    return lz_ledger_next_allowed(&ctx->contacts[contact].outgoing,
                                  &ctx->contacts[contact].peer_rules, now);
}

void lz_wifi(const lz_ctx *ctx, const uint8_t **name, size_t *name_len, const uint8_t **password,
             size_t *password_len)
{
    *name = ctx->wifi_name;
    *name_len = ctx->wifi_name_len;
    *password = ctx->wifi_password;
    *password_len = ctx->wifi_password_len;
}
