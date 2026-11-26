/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The satellite itself, without I/O: what it does with every message,
 * button and tick. Mirrors SatState in crates/client/src/tests/reference.rs
 * and ContactState in crates/core/src/contact.rs.
 */
#include <string.h>

#include "internal.h"

/* ---- helpers ---- */

int lz_rand(lz_ctx *ctx, uint8_t *buf, size_t len)
{
    if (!ctx->pf.random || ctx->pf.random(ctx->pf.user, buf, len) != 0) {
        lz_wipe(buf, len);
        return LZ_E_CRYPTO;
    }
    return LZ_OK;
}

void lz_event_push(lz_ctx *ctx, int type, int contact)
{
    lz_event *e;
    if (ctx->ev_count == LZ_MAX_EVENTS) {
        /* Drop the oldest; the view always shows the current state. */
        ctx->ev_head = (uint8_t)((ctx->ev_head + 1) % LZ_MAX_EVENTS);
        ctx->ev_count--;
    }
    e = &ctx->events[(ctx->ev_head + ctx->ev_count) % LZ_MAX_EVENTS];
    ctx->ev_count++;
    memset(e, 0, sizeof *e);
    e->type = type;
    e->contact = 0xFF;
    if (contact >= 0 && contact < ctx->ncontacts) {
        e->contact = (uint8_t)contact;
        e->name = ctx->contacts[contact].name;
    }
}

lz_event *lz_event_last(lz_ctx *ctx)
{
    return &ctx->events[(ctx->ev_head + ctx->ev_count - 1) % LZ_MAX_EVENTS];
}

int lz_sat_contact_index(const lz_ctx *ctx, const uint8_t mailbox[32])
{
    int i;
    for (i = 0; i < ctx->ncontacts; i++) {
        if (memcmp(ctx->contacts[i].own_mailbox, mailbox, 32) == 0) return i;
    }
    return -1;
}

static int url_eq(const lz_url *a, const lz_url *b)
{
    return a->len == b->len && memcmp(a->b, b->b, a->len) == 0;
}

static int relay_used(const lz_ctx *ctx, int r)
{
    int i;
    if (ctx->channel.relay == r) return 1;
    for (i = 0; i < ctx->ncontacts; i++) {
        if (ctx->contacts[i].own_relay == r) return 1;
    }
    return 0;
}

int lz_sat_relay_index(lz_ctx *ctx, const lz_url *url, int add)
{
    int i;
    for (i = 0; i < ctx->nrelays; i++) {
        if (url_eq(&ctx->relays[i].url, url)) return i;
    }
    if (!add) return -1;
    for (i = 0; i < ctx->nrelays; i++) {
        if (!relay_used(ctx, i)) break;
    }
    if (i == ctx->nrelays) {
        if (ctx->nrelays == LZ_MAX_RELAYS) return -1;
        ctx->nrelays++;
    }
    memset(&ctx->relays[i], 0, sizeof ctx->relays[i]);
    ctx->relays[i].url = *url;
    return i;
}

int lz_sat_contact_keys(lz_ctx *ctx, int ci)
{
    lz_contact *c = &ctx->contacts[ci];
    if (c->keys_ready) return LZ_OK;
    if (lz_scalarmult_base(c->own_pk, c->secret_key) != 0 ||
        lz_box_beforenm(c->shared, c->peer_key, c->secret_key) != 0) {
        lz_wipe(c->shared, 32);
        return LZ_E_CRYPTO;
    }
    c->keys_ready = 1;
    return LZ_OK;
}

static void next_stamp(lz_ctx *ctx, lz_stamp *out, const lz_stamp *last)
{
    lz_stamp s;
    if (lz_rand(ctx, s.id, 16) != LZ_OK) memset(s.id, 0xFF, 16);
    s.at = lz_max64(ctx->now, last->at);
    if (lz_stamp_cmp(&s, last) <= 0) s.at = lz_sat_add(last->at, 1);
    *out = s;
}

static int outbox_free(const lz_ctx *ctx) { return LZ_OUTBOX_SIZE - ctx->noutbox; }

static lz_out_entry *outbox_slot(lz_ctx *ctx)
{
    lz_out_entry *o;
    if (ctx->noutbox == LZ_OUTBOX_SIZE) return NULL;
    o = &ctx->outbox[ctx->noutbox];
    memset(o, 0, sizeof *o);
    o->created_at = ctx->now;
    return o;
}

/* Seals m (with its identifier set) to contact ci and queues it. */
static int queue_peer(lz_ctx *ctx, int ci, const lz_message *m)
{
    lz_contact *c = &ctx->contacts[ci];
    lz_out_entry *o = outbox_slot(ctx);
    uint8_t nonce[24];
    int err;
    if (!o) return LZ_E_FULL;
    if ((err = lz_sat_contact_keys(ctx, ci)) != LZ_OK) return err;
    if ((err = lz_rand(ctx, nonce, sizeof nonce)) != LZ_OK) return err;
    if ((err = lz_seal_a(o->env, m, c->shared, c->own_pk, c->peer_key, nonce)) != LZ_OK) {
        return err;
    }
    memcpy(o->contact, c->own_mailbox, 32);
    o->kind = m->type;
    ctx->noutbox++;
    ctx->dirty = 1;
    return LZ_OK;
}

static int queue_up(lz_ctx *ctx, const lz_message *m)
{
    lz_out_entry *o = outbox_slot(ctx);
    uint8_t nonce[24];
    int err;
    if (!o) return LZ_E_FULL;
    if ((err = lz_rand(ctx, nonce, sizeof nonce)) != LZ_OK) return err;
    if ((err = lz_seal_c(o->env, m, ctx->channel.key, nonce)) != LZ_OK) return err;
    o->to_channel = 1;
    o->kind = m->type;
    ctx->noutbox++;
    ctx->dirty = 1;
    return LZ_OK;
}

/* A new message of this type, now, with a random identifier. */
static int new_message(lz_ctx *ctx, lz_message *m, uint8_t type)
{
    memset(m, 0, sizeof *m);
    m->type = type;
    m->sent_at = ctx->now;
    return lz_rand(ctx, m->id, 16);
}

static int up_answered(lz_ctx *ctx, int ci, const uint8_t request[16], uint8_t kind,
                       const lz_reason *reason)
{
    lz_message m;
    int err = new_message(ctx, &m, LZ_T_SAT_ANSWERED);
    if (err != LZ_OK) return err;
    memcpy(m.u.responded.contact, ctx->contacts[ci].own_mailbox, 32);
    memcpy(m.u.responded.request, request, 16);
    m.u.responded.kind = kind;
    if (kind == LZ_KIND_REPORTED) m.u.responded.reason = *reason;
    m.u.responded.at = ctx->now;
    return queue_up(ctx, &m);
}

/* ---- state slots (M8c) ---- */

int lz_sat_current_state(const lz_ctx *ctx, uint64_t now, lz_state_slot *out)
{
    int own = ctx->own_state.has && now < ctx->own_state.until;
    int handed = ctx->handed.has && now < ctx->handed.until;
    if (handed && (!own || ctx->handed.since > ctx->own_state.since)) {
        *out = ctx->handed;
        return 2;
    }
    if (own) {
        *out = ctx->own_state;
        return 1;
    }
    memset(out, 0, sizeof *out);
    return 0;
}

static int apply_share(lz_ctx *ctx, int shared, const lz_reason *reason, uint64_t until,
                       const lz_stamp *stamp)
{
    if (lz_stamp_cmp(stamp, &ctx->share_stamp) <= 0) return 0;
    ctx->share_stamp = *stamp;
    /* All shared states of a person form one. */
    if (ctx->own_state.has && ctx->own_state.shared) memset(&ctx->own_state, 0, sizeof ctx->own_state);
    memset(&ctx->handed, 0, sizeof ctx->handed);
    if (shared) {
        ctx->handed.has = 1;
        ctx->handed.reason = *reason;
        ctx->handed.until = until;
        ctx->handed.since = stamp->at;
    }
    return 1;
}

/* Ends the own state; a shared one is cleared on the full devices too. */
static void end_own_state(lz_ctx *ctx)
{
    int shared = ctx->own_state.shared;
    memset(&ctx->own_state, 0, sizeof ctx->own_state);
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    if (shared) {
        lz_message m;
        memset(&m, 0, sizeof m);
        next_stamp(ctx, &ctx->share_stamp, &ctx->share_stamp);
        m.type = LZ_T_SAT_STATE_CLEARED;
        memcpy(m.id, ctx->share_stamp.id, 16);
        m.sent_at = ctx->share_stamp.at;
        memset(&ctx->handed, 0, sizeof ctx->handed);
        if (queue_up(ctx, &m) != LZ_OK) lz_event_push(ctx, LZ_EV_OUTBOX_FULL, -1);
    }
}

static void begin_own_state(lz_ctx *ctx, const lz_reason *reason, uint32_t minutes, int share)
{
    uint64_t until = lz_sat_add(ctx->now, (uint64_t)minutes * 60u);
    memset(&ctx->own_state, 0, sizeof ctx->own_state);
    ctx->own_state.has = 1;
    ctx->own_state.reason = *reason;
    ctx->own_state.until = until;
    ctx->own_state.since = ctx->now;
    ctx->own_state.shared = share ? 1 : 0;
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    if (share) {
        lz_message m;
        memset(&m, 0, sizeof m);
        next_stamp(ctx, &ctx->share_stamp, &ctx->share_stamp);
        m.type = LZ_T_SAT_STATE_SHARED;
        memcpy(m.id, ctx->share_stamp.id, 16);
        m.sent_at = ctx->share_stamp.at;
        m.u.state.reason = *reason;
        m.u.state.until = until;
        memset(&ctx->handed, 0, sizeof ctx->handed);
        if (queue_up(ctx, &m) != LZ_OK) lz_event_push(ctx, LZ_EV_OUTBOX_FULL, -1);
    }
}

void lz_sat_long_press(lz_ctx *ctx)
{
    if (ctx->own_state.has && ctx->now < ctx->own_state.until) {
        end_own_state(ctx);
        return;
    }
    if (ctx->settings.preset_minutes == 0) return;
    begin_own_state(ctx, &ctx->settings.reason, ctx->settings.preset_minutes,
                    ctx->settings.share);
}

int lz_sat_set_state(lz_ctx *ctx, const lz_reason *reason, uint32_t minutes, int share)
{
    if (minutes == 0 || reason->len > LZ_MAX_REASON_LEN || !lz_utf8_valid(reason->b, reason->len)) {
        return LZ_E_ARG;
    }
    /* A shared state that is replaced by one that is not shared ends on
     * the full devices first. */
    if (!share && ctx->own_state.has && ctx->now < ctx->own_state.until && ctx->own_state.shared) {
        end_own_state(ctx);
    }
    begin_own_state(ctx, reason, minutes, share);
    return LZ_OK;
}

int lz_sat_clear_state(lz_ctx *ctx)
{
    if (!(ctx->own_state.has && ctx->now < ctx->own_state.until)) return LZ_E_STATE;
    end_own_state(ctx);
    return LZ_OK;
}

/* ---- contacts ---- */

void lz_sat_remove_contact(lz_ctx *ctx, int ci)
{
    if (ci < 0 || ci >= ctx->ncontacts) return;
    /* An HTTP job for its mailbox finds nothing when it returns. */
    lz_wipe(&ctx->contacts[ci], sizeof ctx->contacts[ci]);
    memmove(&ctx->contacts[ci], &ctx->contacts[ci + 1],
            (size_t)(ctx->ncontacts - ci - 1) * sizeof ctx->contacts[0]);
    memmove(&ctx->rt[1 + ci], &ctx->rt[2 + ci], (size_t)(ctx->ncontacts - ci - 1) * sizeof ctx->rt[0]);
    ctx->ncontacts--;
    lz_wipe(&ctx->contacts[ctx->ncontacts], sizeof ctx->contacts[0]);
    memset(&ctx->rt[1 + ctx->ncontacts], 0, sizeof ctx->rt[0]);
    ctx->dirty = 1;
    ctx->view_dirty = 1;
}

int lz_sat_add_contact(lz_ctx *ctx, const uint8_t mailbox[32], const lz_contact_record *r)
{
    lz_contact *c;
    uint64_t times[LZ_RECENT_MAX];
    uint8_t ntimes = r->recent_len, i;
    int relay, ci;
    if (ctx->ncontacts == LZ_MAX_CONTACTS) return LZ_E_LIMIT;
    if (lz_sat_contact_index(ctx, mailbox) >= 0) return LZ_E_STATE;
    if (memcmp(mailbox, ctx->channel.mailbox, 32) == 0) return LZ_E_VALUE;
    if (!lz_url_ok(&r->own_relay) || !lz_url_ok(&r->peer_relay)) return LZ_E_VALUE;
    if (lz_sat_relay_index(ctx, &r->own_relay, 0) < 0 && ctx->nrelays == LZ_MAX_RELAYS) {
        int k, free_slot = 0;
        for (k = 0; k < ctx->nrelays; k++) {
            if (!relay_used(ctx, k)) free_slot = 1;
        }
        if (!free_slot) return LZ_E_LIMIT;
    }
    ci = ctx->ncontacts;
    c = &ctx->contacts[ci];
    memset(c, 0, sizeof *c);
    memcpy(c->secret_key, r->secret_key, 32);
    memcpy(c->peer_key, r->peer_key, 32);
    if (lz_sat_contact_keys(ctx, ci) != LZ_OK) {
        lz_wipe(c, sizeof *c);
        return LZ_E_CRYPTO;
    }
    relay = lz_sat_relay_index(ctx, &r->own_relay, 1);
    if (relay < 0) {
        lz_wipe(c, sizeof *c);
        return LZ_E_LIMIT;
    }
    memcpy(c->own_mailbox, mailbox, 32);
    c->own_relay = (uint8_t)relay;
    memcpy(c->enrolment, r->enrolment, 32);
    c->name = r->name;
    memcpy(c->peer_mailbox, r->peer_mailbox, 32);
    memcpy(c->peer_send_token, r->peer_send_token, 32);
    c->peer_relay = r->peer_relay;
    c->own_rules = r->own_rules;
    c->own_rules_stamp = r->own_rules_stamp;
    c->peer_rules = r->peer_rules;
    c->peer_rules_stamp = r->peer_rules_stamp;
    c->flags = r->flags;
    /* The own requests: counted ones without identifier, and the open one
     * (contact_from in the reference satellite). */
    memcpy(times, r->recent, sizeof times);
    if (ntimes > LZ_RECENT_MAX) ntimes = LZ_RECENT_MAX;
    if (!lz_is_zero(r->open_id, 16)) {
        for (i = 0; i < ntimes; i++) {
            if (times[i] == r->open_at) {
                memmove(&times[i], &times[i + 1], (size_t)(ntimes - i - 1) * sizeof times[0]);
                ntimes--;
                break;
            }
        }
        lz_ledger_record(&c->outgoing, r->open_id, r->open_at);
    }
    for (i = 0; i < ntimes; i++) {
        lz_entry e;
        memset(&e, 0, sizeof e);
        e.at = times[i];
        e.answered = 1;
        e.answered_at = times[i];
        lz_ledger_insert(&c->outgoing, &e);
    }
    if (r->last_answer != 0) {
        c->outgoing.has_last_answer = 1;
        c->outgoing.last_answer = r->last_answer;
    }
    ctx->ncontacts++;
    memset(&ctx->rt[1 + ci], 0, sizeof ctx->rt[0]);
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    return LZ_OK;
}

/* ---- from a contact (form A) ---- */

static int accept_request(lz_contact *c, const uint8_t id[16], uint64_t at, uint64_t now)
{
    const lz_rules *rules = &c->own_rules;
    int pos = lz_ledger_find(&c->incoming, id);
    if (pos >= 0) {
        /* Another own device answered it before it arrived here; it now
         * gets its real time. */
        lz_entry e = c->incoming.e[pos];
        int open;
        lz_ledger_remove(&c->incoming, pos);
        e.at = at;
        lz_ledger_insert(&c->incoming, &e);
        pos = lz_ledger_open(&c->incoming, rules, now);
        open = pos >= 0 && memcmp(c->incoming.e[pos].id, id, 16) == 0;
        return !e.answered && open;
    }
    lz_ledger_prune(&c->incoming, rules, now);
    if (lz_ledger_next_allowed(&c->incoming, rules, now) > now) return 0;
    if (lz_sat_add(at, (uint64_t)rules->expiry_minutes * 60u) <= now) return 0;
    lz_ledger_record(&c->incoming, id, at);
    return 1;
}

static int accept_response(lz_contact *c, const uint8_t request[16], uint8_t kind, uint64_t at)
{
    /* Valid if sent while the request was current, even if late. */
    int i = lz_ledger_current(&c->outgoing, request, &c->peer_rules, at);
    if (i < 0 || c->outgoing.e[i].answered) return 0;
    if (kind == LZ_KIND_MANUAL) {
        lz_ledger_answer(&c->outgoing, request, at);
    } else {
        if (c->outgoing.e[i].reported) return 0;
        c->outgoing.e[i].reported = 1;
    }
    return 1;
}

void lz_sat_receive_contact(lz_ctx *ctx, int ci, const uint8_t *env, size_t len)
{
    lz_contact *c = &ctx->contacts[ci];
    uint64_t now = ctx->now, at;
    lz_message m;
    if (c->ended || lz_sat_contact_keys(ctx, ci) != LZ_OK) return;
    if (lz_open_a(&m, env, len, c->shared, c->own_pk, c->peer_key) != LZ_OK) return;
    if (!lz_in_window(m.sent_at, now)) return;
    if (lz_remember(c->seen, &c->seen_n, LZ_MAX_SEEN, &c->seen_floor, m.id, m.sent_at, now)) {
        return;
    }
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    /* Times from the other side are capped at the own clock. */
    at = lz_min64(m.sent_at, now);
    switch (m.type) {
    case LZ_T_RULES: {
        /* Newest wins, by the uncapped sending time. */
        lz_stamp s;
        s.at = m.sent_at;
        memcpy(s.id, m.id, 16);
        if (lz_stamp_cmp(&s, &c->peer_rules_stamp) > 0) {
            c->peer_rules = m.u.rules;
            c->peer_rules_stamp = s;
        }
        break;
    }
    case LZ_T_REQUEST:
        accept_request(c, m.id, at, now);
        break;
    case LZ_T_RESPONSE:
        if (accept_response(c, m.u.response.request, m.u.response.kind, at)) {
            lz_event *e;
            c->answer_shown = 1;
            c->answer_kind = m.u.response.kind;
            c->answer_reason = m.u.response.reason;
            c->answer_at = at;
            c->answer_until = lz_sat_add(now, LZ_ANSWER_SHOWN_SECS);
            lz_event_push(ctx, LZ_EV_ANSWER, ci);
            e = lz_event_last(ctx);
            e->kind = m.u.response.kind;
            e->reason = m.u.response.reason;
            e->at = at;
        }
        break;
    case LZ_T_END:
        c->ended = 1;
        lz_event_push(ctx, LZ_EV_CONTACT_ENDED, ci);
        break;
    default:
        /* Pairing types, messages between own devices, unknown types. */
        break;
    }
}

/* ---- from the channel (form C, down types only) ---- */

static const uint8_t *down_contact(const lz_message *m)
{
    switch (m->type) {
    case LZ_T_HOME_RESPONDED:
        return m->u.responded.contact;
    case LZ_T_HOME_REQUESTED:
        return m->u.requested.contact;
    case LZ_T_HOME_RULES:
        return m->u.rules_set.contact;
    case LZ_T_HOME_CONTACT_SETTINGS:
        return m->u.contact_settings.contact;
    case LZ_T_HOME_CONTACT_PART:
        return m->u.part.contact;
    default:
        return NULL;
    }
}

static void park(lz_ctx *ctx, const uint8_t plain[LZ_PLAINTEXT_LEN])
{
    if (ctx->nparked == LZ_MAX_PARKED) {
        lz_wipe(&ctx->parked[0], sizeof ctx->parked[0]);
        memmove(&ctx->parked[0], &ctx->parked[1], (LZ_MAX_PARKED - 1) * sizeof ctx->parked[0]);
        ctx->nparked--;
    }
    memcpy(ctx->parked[ctx->nparked].plain, plain, LZ_PLAINTEXT_LEN);
    ctx->parked[ctx->nparked].arrived = ctx->now;
    ctx->nparked++;
    ctx->dirty = 1;
}

static void apply_down(lz_ctx *ctx, const lz_message *m, const uint8_t *plain);

static void apply_part(lz_ctx *ctx, const lz_message *m)
{
    lz_transfer *t = NULL;
    uint8_t *bytes;
    lz_contact_record rec;
    size_t total = 0;
    int i, ci, have = 0, same;
    uint8_t idx = m->u.part.index;

    for (i = 0; i < LZ_MAX_TRANSFERS; i++) {
        lz_transfer *x = &ctx->transfers[i];
        if (x->used && memcmp(x->contact, m->u.part.contact, 32) == 0 &&
            memcmp(x->transfer, m->u.part.transfer, 16) == 0) {
            t = x;
            break;
        }
    }
    if (!t) {
        for (i = 0; i < LZ_MAX_TRANSFERS; i++) {
            if (!ctx->transfers[i].used) {
                t = &ctx->transfers[i];
                break;
            }
        }
        if (!t) {
            /* Full: the oldest transfer gives way. */
            t = &ctx->transfers[0];
            for (i = 1; i < LZ_MAX_TRANSFERS; i++) {
                if (ctx->transfers[i].first_seen < t->first_seen) t = &ctx->transfers[i];
            }
        }
        lz_wipe(t, sizeof *t);
        t->used = 1;
        memcpy(t->contact, m->u.part.contact, 32);
        memcpy(t->transfer, m->u.part.transfer, 16);
        t->count = m->u.part.count;
        t->first_seen = ctx->now;
    }
    ctx->dirty = 1;
    same = idx < LZ_RECORD_MAX_PARTS && (t->have & (1u << idx));
    if (t->discarded || m->u.part.count != t->count || idx >= m->u.part.count ||
        m->u.part.count > LZ_RECORD_MAX_PARTS ||
        (same && (t->lens[idx] != m->u.part.len ||
                  memcmp(t->data[idx], m->u.part.data, m->u.part.len) != 0))) {
        /* The whole transfer is discarded; later parts of it too. */
        uint8_t contact[32], transfer[16];
        uint64_t first = t->first_seen;
        uint8_t count = t->count;
        memcpy(contact, t->contact, 32);
        memcpy(transfer, t->transfer, 16);
        lz_wipe(t, sizeof *t);
        t->used = 1;
        t->discarded = 1;
        t->count = count;
        t->first_seen = first;
        memcpy(t->contact, contact, 32);
        memcpy(t->transfer, transfer, 16);
        return;
    }
    if (!same) {
        memcpy(t->data[idx], m->u.part.data, m->u.part.len);
        t->lens[idx] = m->u.part.len;
        t->have = (uint8_t)(t->have | (1u << idx));
    }
    for (i = 0; i < t->count; i++) {
        if (t->have & (1u << i)) have++;
    }
    if (have < t->count) return;
    /* The parts in order, moved together in place (the parts are
     * contiguous in t->data, and each moves only towards the start). */
    bytes = (uint8_t *)(void *)t->data; /* the whole array, as bytes */
    for (i = 0; i < t->count; i++) {
        memmove(bytes + total, t->data[i], t->lens[i]);
        total += t->lens[i];
    }
    i = lz_contact_record_decode(&rec, bytes, total);
    lz_wipe(t, sizeof *t);
    if (i != LZ_OK) return;
    ci = lz_sat_contact_index(ctx, m->u.part.contact);
    if (ci >= 0) {
        /* Known: the name, and rules whose stamp is newer; the settings in
         * the record carry no stamp and stay. */
        lz_contact *c = &ctx->contacts[ci];
        c->name = rec.name;
        if (lz_stamp_cmp(&rec.own_rules_stamp, &c->own_rules_stamp) > 0) {
            c->own_rules = rec.own_rules;
            c->own_rules_stamp = rec.own_rules_stamp;
        }
        if (lz_stamp_cmp(&rec.peer_rules_stamp, &c->peer_rules_stamp) > 0) {
            c->peer_rules = rec.peer_rules;
            c->peer_rules_stamp = rec.peer_rules_stamp;
        }
        lz_event_push(ctx, LZ_EV_CONTACT_UPDATED, ci);
    } else if (lz_sat_add_contact(ctx, m->u.part.contact, &rec) != LZ_OK) {
        lz_event *e;
        lz_event_push(ctx, LZ_EV_CONTACT_REFUSED, -1);
        e = lz_event_last(ctx);
        e->name = rec.name;
    } else {
        int p = 0;
        ci = ctx->ncontacts - 1;
        lz_event_push(ctx, LZ_EV_CONTACT_ADDED, ci);
        /* Down messages that waited for it, in their order. */
        while (p < ctx->nparked) {
            lz_message pm;
            const uint8_t *pc;
            if (lz_message_decode(&pm, ctx->parked[p].plain, LZ_PLAINTEXT_LEN) == LZ_OK &&
                (pc = down_contact(&pm)) != NULL && memcmp(pc, m->u.part.contact, 32) == 0) {
                lz_wipe(&ctx->parked[p], sizeof ctx->parked[p]);
                memmove(&ctx->parked[p], &ctx->parked[p + 1],
                        (size_t)(ctx->nparked - p - 1) * sizeof ctx->parked[0]);
                ctx->nparked--;
                apply_down(ctx, &pm, NULL);
            } else {
                p++;
            }
        }
    }
    lz_wipe(&rec, sizeof rec);
}

static void apply_down(lz_ctx *ctx, const lz_message *m, const uint8_t *plain)
{
    lz_stamp stamp;
    const uint8_t *contact = down_contact(m);
    lz_contact *c = NULL;
    stamp.at = m->sent_at;
    memcpy(stamp.id, m->id, 16);
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    if (contact && m->type != LZ_T_HOME_CONTACT_PART) {
        int ci = lz_sat_contact_index(ctx, contact);
        if (ci < 0) {
            if (plain) park(ctx, plain);
            return;
        }
        c = &ctx->contacts[ci];
    }
    switch (m->type) {
    case LZ_T_HOME_CONTACT_PART:
        apply_part(ctx, m);
        break;
    case LZ_T_HOME_RESPONDED: {
        /* Another device answered. */
        uint64_t at = lz_min64(m->u.responded.at, ctx->now);
        const uint8_t *req = m->u.responded.request;
        if (lz_ledger_find(&c->incoming, req) < 0) lz_ledger_record(&c->incoming, req, at);
        if (m->u.responded.kind == LZ_KIND_MANUAL) {
            lz_ledger_answer(&c->incoming, req, at);
        } else {
            int i;
            for (i = 0; i < c->incoming.n; i++) {
                if (memcmp(c->incoming.e[i].id, req, 16) == 0 && !c->incoming.e[i].answered) {
                    c->incoming.e[i].reported = 1;
                    break;
                }
            }
        }
        break;
    }
    case LZ_T_HOME_REQUESTED:
        /* A full device asked the contact. */
        lz_ledger_prune(&c->outgoing, &c->peer_rules, ctx->now);
        if (lz_ledger_find(&c->outgoing, m->u.requested.request) < 0) {
            lz_ledger_record(&c->outgoing, m->u.requested.request,
                             lz_min64(m->u.requested.at, ctx->now));
        }
        break;
    case LZ_T_HOME_RULES:
        if (lz_stamp_cmp(&stamp, &c->own_rules_stamp) > 0) {
            c->own_rules = m->u.rules_set.rules;
            c->own_rules_stamp = stamp;
        }
        break;
    case LZ_T_HOME_CONTACT_SETTINGS:
        if (lz_stamp_cmp(&stamp, &c->flags_stamp) > 0) {
            c->flags = m->u.contact_settings.flags;
            c->flags_stamp = stamp;
        }
        break;
    case LZ_T_HOME_SETTINGS:
        if (lz_stamp_cmp(&stamp, &ctx->settings_stamp) > 0) {
            ctx->settings = m->u.settings;
            ctx->settings_stamp = stamp;
        }
        break;
    case LZ_T_HOME_STATE_SHARED:
        apply_share(ctx, 1, &m->u.state.reason, m->u.state.until, &stamp);
        break;
    case LZ_T_HOME_STATE_CLEARED:
        apply_share(ctx, 0, NULL, 0, &stamp);
        break;
    case LZ_T_HOME_SEND_TOKEN:
        /* The last one received wins (m10-satellite.md). */
        memcpy(ctx->channel.send_token, m->u.send_token, 32);
        break;
    default:
        break;
    }
}

void lz_sat_receive_channel(lz_ctx *ctx, const uint8_t *env, size_t len)
{
    uint8_t plain[LZ_PLAINTEXT_LEN];
    lz_message m;
    lz_sat_expire(ctx);
    if (lz_open_plain(plain, env, len, ctx->channel.key) != LZ_OK) return;
    if (lz_message_decode(&m, plain, sizeof plain) != LZ_OK) goto out;
    if (!lz_in_window(m.sent_at, ctx->now)) goto out;
    /* One way: anything but a down type is dropped, before it is
     * remembered. */
    if (!lz_type_is_down(m.type)) goto out;
    if (lz_remember(ctx->channel.seen, &ctx->channel.seen_n, LZ_MAX_CHANNEL_SEEN,
                    &ctx->channel.seen_floor, m.id, m.sent_at, ctx->now)) {
        goto out;
    }
    ctx->dirty = 1;
    apply_down(ctx, &m, plain);
out:
    lz_wipe(plain, sizeof plain);
    lz_wipe(&m, sizeof m);
}

/* ---- buttons and timers ---- */

void lz_sat_press(lz_ctx *ctx)
{
    int i;
    /* The press dismisses the answers shown. */
    for (i = 0; i < ctx->ncontacts; i++) ctx->contacts[i].answer_shown = 0;
    ctx->dirty = 1;
    ctx->view_dirty = 1;
    for (i = 0; i < ctx->ncontacts; i++) {
        lz_contact *c = &ctx->contacts[i];
        lz_message m;
        uint8_t request[16];
        int open;
        lz_event *e;
        if (c->ended) continue;
        open = lz_ledger_open(&c->incoming, &c->own_rules, ctx->now);
        if (open < 0) continue;
        if (outbox_free(ctx) < 2) {
            lz_event_push(ctx, LZ_EV_OUTBOX_FULL, i);
            break;
        }
        memcpy(request, c->incoming.e[open].id, 16);
        if (new_message(ctx, &m, LZ_T_RESPONSE) != LZ_OK) break;
        memcpy(m.u.response.request, request, 16);
        m.u.response.kind = LZ_KIND_MANUAL;
        if (queue_peer(ctx, i, &m) != LZ_OK) continue;
        lz_ledger_answer(&c->incoming, request, ctx->now);
        up_answered(ctx, i, request, LZ_KIND_MANUAL, NULL);
        lz_event_push(ctx, LZ_EV_ANSWERED, i);
        e = lz_event_last(ctx);
        e->kind = LZ_KIND_MANUAL;
        e->at = ctx->now;
    }
}

void lz_sat_ask(lz_ctx *ctx, uint8_t button)
{
    int i, ci = -1;
    lz_contact *c;
    lz_message m;
    uint64_t next;
    for (i = 0; i < ctx->ncontacts && button != 0; i++) {
        if (ctx->contacts[i].flags.ask_button == button && !ctx->contacts[i].ended) {
            ci = i;
            break;
        }
    }
    if (ci < 0) {
        lz_event_push(ctx, LZ_EV_ASK_REFUSED, -1);
        lz_event_last(ctx)->at = UINT64_MAX;
        return;
    }
    c = &ctx->contacts[ci];
    lz_ledger_prune(&c->outgoing, &c->peer_rules, ctx->now);
    ctx->dirty = 1;
    next = lz_ledger_next_allowed(&c->outgoing, &c->peer_rules, ctx->now);
    if (next > ctx->now) {
        lz_event_push(ctx, LZ_EV_ASK_REFUSED, ci);
        lz_event_last(ctx)->at = next;
        return;
    }
    if (outbox_free(ctx) < 2) {
        lz_event_push(ctx, LZ_EV_OUTBOX_FULL, ci);
        return;
    }
    if (new_message(ctx, &m, LZ_T_REQUEST) != LZ_OK) return;
    if (queue_peer(ctx, ci, &m) != LZ_OK) return;
    lz_ledger_record(&c->outgoing, m.id, ctx->now);
    {
        lz_message up;
        if (new_message(ctx, &up, LZ_T_SAT_ASKED) == LZ_OK) {
            memcpy(up.u.requested.contact, c->own_mailbox, 32);
            memcpy(up.u.requested.request, m.id, 16);
            up.u.requested.at = ctx->now;
            queue_up(ctx, &up);
        }
    }
    lz_event_push(ctx, LZ_EV_ASKED, ci);
    lz_event_last(ctx)->at = ctx->now;
}

void lz_sat_expire(lz_ctx *ctx)
{
    uint64_t now = ctx->now;
    int i = 0;
    while (i < ctx->noutbox) {
        if (now >= lz_sat_add(ctx->outbox[i].created_at, LZ_KEEP_SECS)) {
            memmove(&ctx->outbox[i], &ctx->outbox[i + 1],
                    (size_t)(ctx->noutbox - i - 1) * sizeof ctx->outbox[0]);
            ctx->noutbox--;
            memset(&ctx->outbox[ctx->noutbox], 0, sizeof ctx->outbox[0]);
            ctx->dirty = 1;
        } else {
            i++;
        }
    }
    i = 0;
    while (i < ctx->nparked) {
        if (now >= lz_sat_add(ctx->parked[i].arrived, LZ_KEEP_SECS)) {
            lz_wipe(&ctx->parked[i], sizeof ctx->parked[i]);
            memmove(&ctx->parked[i], &ctx->parked[i + 1],
                    (size_t)(ctx->nparked - i - 1) * sizeof ctx->parked[0]);
            ctx->nparked--;
            ctx->dirty = 1;
        } else {
            i++;
        }
    }
    for (i = 0; i < LZ_MAX_TRANSFERS; i++) {
        if (ctx->transfers[i].used && now >= lz_sat_add(ctx->transfers[i].first_seen, LZ_KEEP_SECS)) {
            lz_wipe(&ctx->transfers[i], sizeof ctx->transfers[i]);
            ctx->dirty = 1;
        }
    }
    for (i = 0; i < ctx->ncontacts; i++) {
        lz_contact *c = &ctx->contacts[i];
        if (c->answer_shown && now >= c->answer_until) {
            c->answer_shown = 0;
            ctx->dirty = 1;
            ctx->view_dirty = 1;
        }
    }
}

void lz_sat_tick(lz_ctx *ctx)
{
    lz_state_slot state;
    int i;
    lz_sat_expire(ctx);
    if (!lz_sat_current_state(ctx, ctx->now, &state)) return;
    for (i = 0; i < ctx->ncontacts; i++) {
        lz_contact *c = &ctx->contacts[i];
        lz_entry *e;
        lz_message m;
        uint64_t due;
        int open;
        if (!c->flags.reported || c->ended) continue;
        open = lz_ledger_open(&c->incoming, &c->own_rules, ctx->now);
        if (open < 0) continue;
        e = &c->incoming.e[open];
        if (e->reported) continue;
        /* The waiting time runs from the time of the request, so that it
         * survives a restart. */
        due = lz_sat_add(e->at, (uint64_t)ctx->settings.waiting_minutes * 60u);
        if (due > ctx->now) continue;
        if (outbox_free(ctx) < 2) {
            lz_event_push(ctx, LZ_EV_OUTBOX_FULL, i);
            break;
        }
        if (new_message(ctx, &m, LZ_T_RESPONSE) != LZ_OK) break;
        memcpy(m.u.response.request, e->id, 16);
        m.u.response.kind = LZ_KIND_REPORTED;
        m.u.response.reason = state.reason;
        if (queue_peer(ctx, i, &m) != LZ_OK) continue;
        e->reported = 1;
        up_answered(ctx, i, m.u.response.request, LZ_KIND_REPORTED, &state.reason);
        lz_event_push(ctx, LZ_EV_ANSWERED, i);
        lz_event_last(ctx)->kind = LZ_KIND_REPORTED;
        lz_event_last(ctx)->reason = state.reason;
        lz_event_last(ctx)->at = ctx->now;
    }
}

/* Events for requests that opened or closed, and for the state. */
void lz_sat_track_open(lz_ctx *ctx)
{
    lz_state_slot state;
    int i, has;
    for (i = 0; i < ctx->ncontacts; i++) {
        lz_contact *c = &ctx->contacts[i];
        int open = c->ended ? -1 : lz_ledger_open(&c->incoming, &c->own_rules, ctx->now);
        if (open >= 0) {
            const uint8_t *id = c->incoming.e[open].id;
            if (!c->was_open || memcmp(c->was_open_id, id, 16) != 0) {
                if (c->was_open) lz_event_push(ctx, LZ_EV_REQUEST_CLOSED, i);
                lz_event_push(ctx, LZ_EV_REQUEST_OPENED, i);
                lz_event_last(ctx)->at = c->incoming.e[open].at;
                c->was_open = 1;
                memcpy(c->was_open_id, id, 16);
                ctx->view_dirty = 1;
            }
        } else if (c->was_open) {
            c->was_open = 0;
            memset(c->was_open_id, 0, 16);
            lz_event_push(ctx, LZ_EV_REQUEST_CLOSED, i);
            ctx->view_dirty = 1;
        }
    }
    has = lz_sat_current_state(ctx, ctx->now, &state) != 0;
    if (has != ctx->state_last.has ||
        (has && (state.since != ctx->state_last.since || state.until != ctx->state_last.until ||
                 state.reason.len != ctx->state_last.reason.len ||
                 memcmp(state.reason.b, ctx->state_last.reason.b, state.reason.len) != 0))) {
        ctx->state_last = state;
        ctx->state_last.has = (uint8_t)has;
        lz_event_push(ctx, LZ_EV_STATE, -1);
        if (has) {
            lz_event_last(ctx)->reason = state.reason;
            lz_event_last(ctx)->at = state.until;
        }
        ctx->view_dirty = 1;
    }
}

void lz_sat_wipe(lz_ctx *ctx)
{
    lz_platform pf = ctx->pf;
    lz_wipe(ctx, sizeof *ctx);
    ctx->pf = pf;
    ctx->removed = 1;
    ctx->erase = 1;
    ctx->view_dirty = 1;
    ctx->led_sent = 0xFF; /* sends the light once more: off */
    lz_event_push(ctx, LZ_EV_REMOVED, -1);
}
