/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * A fake relay for the scenario tests: it answers the core's HTTP requests
 * and WebSocket frames as the real relay does (wire-format.md, "Relay
 * interface"), with token checks, the subscribe order of checks, seq and
 * ack, so that the tests drive lz_core only through lz_feed and lz_poll.
 */
#include <stdlib.h>
#include <string.h>

#include "test.h"

void fr_init(fake_relay *fr, t_world *w, lz_ctx *ctx)
{
    memset(fr, 0, sizeof *fr);
    fr->world = w;
    fr->ctx = ctx;
    fr->online = 1;
    fr->reader_tokens = 1;
}

fr_mailbox *fr_add(fake_relay *fr, const char *relay, const uint8_t id[32],
                   const uint8_t send_token[32], const uint8_t *reader_token)
{
    fr_mailbox *m = &fr->mb[fr->nmb++];
    memset(m, 0, sizeof *m);
    m->exists = 1;
    memcpy(m->id, id, 32);
    snprintf(m->relay, sizeof m->relay, "%s", relay);
    lz_sha256(m->send_hash, send_token, 32);
    if (reader_token) {
        m->has_reader = 1;
        lz_sha256(m->reader_hash, reader_token, 32);
    }
    m->seq0 = 1;
    return m;
}

fr_mailbox *fr_find(fake_relay *fr, const uint8_t id[32])
{
    int i;
    for (i = 0; i < fr->nmb; i++) {
        if (memcmp(fr->mb[i].id, id, 32) == 0) return &fr->mb[i];
    }
    return NULL;
}

/* The core's index for a relay address, or -1. */
static int core_relay(fake_relay *fr, const char *relay)
{
    int i;
    for (i = 0; i < fr->ctx->nrelays; i++) {
        const lz_url *u = &fr->ctx->relays[i].url;
        if (strlen(relay) == u->len && memcmp(relay, u->b, u->len) == 0) return i;
    }
    return -1;
}

static void to_core(fake_relay *fr, int r, const char *text)
{
    lz_received(fr->ctx, (uint8_t)r, text, strlen(text));
}

static void b64(char *out, const uint8_t *in, size_t n) { lz_b64url_encode(out, 400, in, n); }

static void deliver(fake_relay *fr, fr_mailbox *m, int r)
{
    int i;
    for (i = 0; i < m->nmsgs; i++) {
        uint64_t seq = m->seq0 + (uint64_t)i;
        char mb[64], data[400], frame[600];
        if (seq <= m->acked) continue;
        b64(mb, m->id, 32);
        b64(data, m->msgs[i], LZ_ENVELOPE_LEN);
        snprintf(frame, sizeof frame, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":%llu,\"data\":\"%s\"}",
                 mb, (unsigned long long)seq, data);
        to_core(fr, r, frame);
        if (!m->subscribed) break; /* the core may have ended it */
    }
}

static void error_frame(fake_relay *fr, int r, const uint8_t *mailbox, const char *code)
{
    char mb[64], frame[200];
    b64(mb, mailbox, 32);
    snprintf(frame, sizeof frame, "{\"op\":\"error\",\"mailbox\":\"%s\",\"code\":\"%s\"}", mb, code);
    to_core(fr, r, frame);
}

void fr_put(fake_relay *fr, const uint8_t mailbox[32], const uint8_t env[LZ_ENVELOPE_LEN])
{
    fr_mailbox *m = fr_find(fr, mailbox);
    int r;
    if (!m || !m->exists || m->nmsgs == FR_MAX_MESSAGES) return;
    memcpy(m->msgs[m->nmsgs++], env, LZ_ENVELOPE_LEN);
    r = core_relay(fr, m->relay);
    if (m->subscribed && r >= 0 && fr->connected[r]) deliver(fr, m, r);
}

void fr_delete(fake_relay *fr, const uint8_t mailbox[32])
{
    fr_mailbox *m = fr_find(fr, mailbox);
    int r;
    if (!m) return;
    m->exists = 0;
    r = core_relay(fr, m->relay);
    if (m->subscribed && r >= 0 && fr->connected[r]) {
        m->subscribed = 0;
        error_frame(fr, r, m->id, "unknown_mailbox");
    }
}

void fr_revoke(fake_relay *fr, const uint8_t mailbox[32])
{
    fr_mailbox *m = fr_find(fr, mailbox);
    int r;
    if (!m) return;
    m->has_reader = 0;
    r = core_relay(fr, m->relay);
    if (m->subscribed && r >= 0 && fr->connected[r]) {
        m->subscribed = 0;
        error_frame(fr, r, m->id, "unknown_reader");
    }
}

static int get_b64(const t_json *o, const char *key, uint8_t *out, size_t n)
{
    size_t len;
    const char *s = t_json_str(t_json_get(o, key), &len);
    return lz_b64url_decode(out, n, s, len) == LZ_OK;
}

static void client_frame(fake_relay *fr, int r, const char *text, size_t len)
{
    t_json *j = t_json_parse(text, len);
    size_t n;
    const char *op;
    uint8_t mailbox[32], token[32], reader[16], h[32];
    fr_mailbox *m;
    fr->frames_out++;
    snprintf(fr->last_frame, sizeof fr->last_frame, "%.*s", (int)len, text);
    if (!j) return;
    op = t_json_str(t_json_get(j, "op"), &n);
    if (!get_b64(j, "mailbox", mailbox, 32)) goto out;
    m = fr_find(fr, mailbox);
    if (n == 9 && memcmp(op, "subscribe", 9) == 0) {
        if (!get_b64(j, "token", token, 32) || !get_b64(j, "reader", reader, 16)) goto out;
        CHECK_MEM(reader, fr->ctx->reader, 16);
        if (!m || !m->exists || core_relay(fr, m->relay) != r) {
            error_frame(fr, r, mailbox, "unknown_mailbox");
        } else if (!m->has_reader) {
            error_frame(fr, r, mailbox, "unknown_reader");
        } else {
            lz_sha256(h, token, 32);
            if (memcmp(h, m->reader_hash, 32) != 0) {
                error_frame(fr, r, mailbox, "unauthorized");
            } else {
                char mb[64], frame[200];
                m->subscribed = 1;
                b64(mb, mailbox, 32);
                snprintf(frame, sizeof frame, "{\"op\":\"subscribed\",\"mailbox\":\"%s\"}", mb);
                to_core(fr, r, frame);
                deliver(fr, m, r);
            }
        }
    } else if (n == 3 && memcmp(op, "ack", 3) == 0 && m) {
        long long seq = t_json_num(t_json_get(j, "seq"));
        if (seq > 0 && (uint64_t)seq > m->acked) {
            int drop = 0;
            m->acked = (uint64_t)seq;
            while (drop < m->nmsgs && m->seq0 + (uint64_t)drop <= m->acked) drop++;
            memmove(m->msgs[0], m->msgs[drop], (size_t)(m->nmsgs - drop) * LZ_ENVELOPE_LEN);
            m->nmsgs -= drop;
            m->seq0 += (uint64_t)drop;
        }
    }
out:
    t_json_free(j);
}

/* The new token must be in the stored state before it goes out. */
static void check_persisted(fake_relay *fr, const uint8_t mailbox[32], const uint8_t hash[32])
{
    static lz_ctx stored;
    lz_platform pf = fr->ctx->pf;
    uint8_t h[32];
    int i, found = 0;
    CHECK(lz_init(&stored, &pf) == LZ_OK);
    CHECK(lz_load(&stored, fr->blob, fr->blob_len) == LZ_OK);
    if (memcmp(stored.channel.mailbox, mailbox, 32) == 0 && stored.channel.has_new_token) {
        lz_sha256(h, stored.channel.new_token, 32);
        found = memcmp(h, hash, 32) == 0;
    }
    for (i = 0; i < stored.ncontacts; i++) {
        if (memcmp(stored.contacts[i].own_mailbox, mailbox, 32) == 0 &&
            stored.contacts[i].has_new_token) {
            lz_sha256(h, stored.contacts[i].new_token, 32);
            found |= memcmp(h, hash, 32) == 0;
        }
    }
    CHECK(found);
    lz_wipe(&stored, sizeof stored);
}

static int auth_token(const char *auth, uint8_t token[32])
{
    if (!auth || strncmp(auth, "Bearer ", 7) != 0) return 0;
    return lz_b64url_decode(token, 32, auth + 7, strlen(auth + 7)) == LZ_OK;
}

static void http(fake_relay *fr, const lz_output *o)
{
    const char *url = o->url, *path = NULL;
    uint64_t date = (uint64_t)((int64_t)fr->world->now + fr->date_offset);
    int i, status = 500;
    const char *body = "";
    uint8_t token[32], h[32], id[32];
    fr->http_requests++;
    if (!fr->online) {
        lz_http_failed(fr->ctx, o->handle);
        return;
    }
    for (i = 0; i < fr->nmb && !path; i++) {
        size_t n = strlen(fr->mb[i].relay);
        if (strncmp(url, fr->mb[i].relay, n) == 0) path = url + n;
    }
    if (!path) {
        lz_http_failed(fr->ctx, o->handle);
        return;
    }
    if (strcmp(path, "/v1/health") == 0) {
        CHECK(strcmp(o->method, "GET") == 0);
        status = 200;
        body = fr->reader_tokens ? "{\"features\":[\"reader_tokens\"]}" : "";
    } else if (strncmp(path, "/v1/mailboxes/", 14) == 0 && strlen(path) >= 14 + 43 &&
               lz_b64url_decode(id, 32, path + 14, 43) == LZ_OK) {
        fr_mailbox *m = fr_find(fr, id);
        const char *rest = path + 14 + 43;
        if (!m || !m->exists) {
            status = 404;
            body = "{\"error\":\"unknown_mailbox\"}";
        } else if (strcmp(rest, "/messages") == 0) {
            CHECK(strcmp(o->method, "POST") == 0);
            CHECK(strcmp(o->content_type, "application/octet-stream") == 0);
            if (!auth_token(o->authorization, token)) {
                status = 401;
            } else {
                lz_sha256(h, token, 32);
                if (memcmp(h, m->send_hash, 32) != 0) {
                    status = 401;
                    body = "{\"error\":\"unauthorized\"}";
                } else if (o->body_len != LZ_ENVELOPE_LEN) {
                    status = 400;
                } else {
                    status = 202;
                    if (fr->nsent < FR_MAX_SENT) {
                        memcpy(fr->sent[fr->nsent].env, o->body, LZ_ENVELOPE_LEN);
                        memcpy(fr->sent[fr->nsent].mailbox, id, 32);
                        fr->sent[fr->nsent].at = fr->world->now;
                        fr->nsent++;
                    }
                }
            }
        } else if (strncmp(rest, "/readers/", 9) == 0) {
            uint8_t reader[16], hash[32];
            CHECK(strcmp(o->method, "PUT") == 0);
            CHECK(lz_b64url_decode(reader, 16, rest + 9, strlen(rest + 9)) == LZ_OK);
            CHECK_MEM(reader, fr->ctx->reader, 16);
            if (!auth_token(o->authorization, token)) {
                status = 401;
            } else {
                lz_sha256(h, token, 32);
                if (!m->has_reader) {
                    status = 404;
                    body = "{\"error\":\"unknown_reader\"}";
                } else if (memcmp(h, m->reader_hash, 32) != 0) {
                    status = 401;
                    body = "{\"error\":\"unauthorized\"}";
                } else {
                    /* {"token_hash":"<64 hex>"} */
                    const char *p = strstr((const char *)o->body, "\"token_hash\":\"");
                    CHECK(p != NULL);
                    CHECK(strcmp(o->content_type, "application/json") == 0);
                    if (p && t_unhex(hash, 32, p + 14, 64) == 32) {
                        int r = core_relay(fr, m->relay);
                        check_persisted(fr, m->id, hash);
                        memcpy(m->reader_hash, hash, 32);
                        status = 204;
                        /* A token change ends live subscriptions with the
                         * old one. */
                        if (m->subscribed && r >= 0 && fr->connected[r]) {
                            m->subscribed = 0;
                            error_frame(fr, r, m->id, "unauthorized");
                        }
                    } else {
                        status = 400;
                    }
                }
            }
        }
    }
    lz_http_response(fr->ctx, o->handle, status, date, (const uint8_t *)body, strlen(body));
}

void fr_run(fake_relay *fr)
{
    lz_output o;
    int guard = 0;
    while (lz_poll(fr->ctx, &o)) {
        CHECK(++guard < 5000);
        if (guard >= 5000) break;
        switch (o.type) {
        case LZ_OUT_ERASE:
            fr->erases++;
            fr->blob_len = 0;
            memset(fr->connected, 0, sizeof fr->connected);
            break;
        case LZ_OUT_PERSIST:
            fr->persists++;
            CHECK(lz_save_buf(fr->ctx, fr->blob, sizeof fr->blob, &fr->blob_len) == LZ_OK);
            break;
        case LZ_OUT_EVENT:
            if (fr->nevents < 64) fr->events[fr->nevents++] = o.event;
            break;
        case LZ_OUT_VIEW:
            fr->views++;
            break;
        case LZ_OUT_LED:
            fr->led = o.led;
            break;
        case LZ_OUT_CONNECT: {
            int i;
            CHECK(strncmp(o.text, "wss://", 6) == 0);
            CHECK(strstr(o.text, "/v1/stream") != NULL);
            if (!fr->online) {
                lz_connected(fr->ctx, o.relay, 0);
                break;
            }
            fr->connected[o.relay] = 1;
            for (i = 0; i < fr->nmb; i++) {
                if (core_relay(fr, fr->mb[i].relay) == o.relay) fr->mb[i].subscribed = 0;
            }
            lz_connected(fr->ctx, o.relay, 1);
            break;
        }
        case LZ_OUT_DISCONNECT: {
            int i;
            fr->connected[o.relay] = 0;
            for (i = 0; i < fr->nmb; i++) {
                if (core_relay(fr, fr->mb[i].relay) == o.relay) fr->mb[i].subscribed = 0;
            }
            break;
        }
        case LZ_OUT_SEND:
            CHECK(fr->connected[o.relay]);
            client_frame(fr, o.relay, o.text, o.len);
            break;
        case LZ_OUT_HTTP:
            http(fr, &o);
            break;
        default:
            CHECK(0);
        }
    }
}

int fr_events(const fake_relay *fr, int type)
{
    int i, n = 0;
    for (i = 0; i < fr->nevents; i++) n += fr->events[i].type == type;
    return n;
}

void fr_clear_events(fake_relay *fr) { fr->nevents = 0; }

int fr_sent_to(const fake_relay *fr, const uint8_t mailbox[32], int from, int *last)
{
    int i, n = 0;
    for (i = from; i < fr->nsent; i++) {
        if (memcmp(fr->sent[i].mailbox, mailbox, 32) == 0) {
            n++;
            if (last) *last = i;
        }
    }
    return n;
}
