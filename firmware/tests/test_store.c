/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* The persisted blob: a full round trip, and every damage refused. */
#include <stdlib.h>

#include "scen.h"

/* A state with something in every table. */
void store_rich_state(scen *s)
{
    lz_message m;
    lz_contact_record r;
    uint8_t bytes[LZ_RECORD_MAX_LEN];
    scen_init(s, 2, NULL);
    set_settings(s, 15, 480, 0, "asleep");
    set_flags(s, 0, 1, 1);
    request_from(s, 0);
    request_from(s, 1);
    lz_long_press(&s->ctx);
    fr_run(&s->fr);
    m = msg(LZ_T_HOME_STATE_SHARED);
    m.u.state.reason.len = 3;
    memcpy(m.u.state.reason.b, "out", 3);
    m.u.state.until = s->w.now + 600;
    m.sent_at = s->w.now - 5; /* older than the own state, which applies */
    phone_sends(s, &m);
    /* An ask, answered: shown. */
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    {
        lz_message resp = msg(LZ_T_RESPONSE);
        int k;
        for (k = 0; k < s->ctx.contacts[0].outgoing.n; k++) {
            if (!s->ctx.contacts[0].outgoing.e[k].answered) {
                memcpy(resp.u.response.request, s->ctx.contacts[0].outgoing.e[k].id, 16);
            }
        }
        resp.u.response.kind = LZ_KIND_REPORTED;
        resp.u.response.reason.len = 2;
        memcpy(resp.u.response.reason.b, "ok", 2);
        contact_sends(s, 0, &resp);
    }
    /* A parked message and a transfer in progress. */
    scen_keys(s, 2, "Dora");
    m = msg(LZ_T_HOME_REQUESTED);
    memcpy(m.u.requested.contact, s->c[2].own_mailbox, 32);
    phone_sends(s, &m);
    scen_record(s, 2, &r);
    lz_contact_record_encode(&r, bytes);
    m = msg(LZ_T_HOME_CONTACT_PART);
    memcpy(m.u.part.contact, s->c[2].own_mailbox, 32);
    memset(m.u.part.transfer, 0x31, 16);
    m.u.part.count = 2;
    m.u.part.len = LZ_MAX_PART_LEN;
    memcpy(m.u.part.data, bytes, LZ_MAX_PART_LEN);
    phone_sends(s, &m);
    /* Offline: the answers wait in the outbox. */
    s->fr.online = 0;
    lz_online(&s->ctx, 0);
    lz_press(&s->ctx);
    fr_run(&s->fr);
}

static void fix_hash(uint8_t *blob, size_t len)
{
    lz_sha256(blob + len - 32, blob, len - 32);
}

static void round_trip_and_damage(void)
{
    scen *s = &S;
    static uint8_t blob[100000], again[100000], bad[100000];
    static lz_ctx other;
    size_t len, len2, i;
    lz_platform pf;
    store_rich_state(s);
    CHECK(s->ctx.ncontacts == 2);
    CHECK(s->ctx.noutbox > 0 || s->ctx.contacts[0].incoming.n > 0);
    CHECK_EQ(s->ctx.nparked, 1);
    CHECK(s->ctx.transfers[0].used || s->ctx.transfers[1].used);
    CHECK(s->ctx.own_state.has && s->ctx.handed.has);
    CHECK(!s->ctx.contacts[0].answer_shown); /* the press dismissed it */
    CHECK_EQ(s->ctx.noutbox, 4);
    CHECK(lz_save_buf(&s->ctx, blob, sizeof blob, &len) == LZ_OK);
    CHECK(len <= lz_save_max());
    printf("store: blob of this state %zu bytes, at most %zu\n", len, lz_save_max());
    t_platform(&pf, &s->w);
    CHECK(lz_init(&other, &pf) == LZ_OK);
    CHECK(lz_load(&other, blob, len) == LZ_OK);
    CHECK(lz_save_buf(&other, again, sizeof again, &len2) == LZ_OK);
    CHECK_EQ(len2, len);
    CHECK_MEM(again, blob, len);
    CHECK_EQ(other.ncontacts, 2);
    CHECK_MEM(other.contacts[1].secret_key, s->ctx.contacts[1].secret_key, 32);
    CHECK_EQ(other.contacts[0].incoming.n, s->ctx.contacts[0].incoming.n);
    CHECK_EQ(other.noutbox, s->ctx.noutbox);
    /* Too small a buffer fails and leaves nothing. */
    CHECK(lz_save_buf(&s->ctx, again, len - 1, &len2) != LZ_OK);
    CHECK_EQ(len2, 0);

    /* Cut anywhere, or any bit flipped: refused, and nothing installed. */
    for (i = 0; i < len; i++) {
        CHECK(lz_load(&other, blob, i) != LZ_OK);
        CHECK(!other.installed);
    }
    for (i = 0; i < len; i += 7) {
        memcpy(bad, blob, len);
        bad[i] ^= (uint8_t)(1u << (i % 8));
        CHECK(lz_load(&other, bad, len) != LZ_OK);
        CHECK(!other.installed);
    }
    /* With the hash fixed up: limits and indices are still checked. */
    memcpy(bad, blob, len);
    bad[4] = 2;
    fix_hash(bad, len);
    CHECK(lz_load(&other, bad, len) == LZ_E_VERSION);
    memcpy(bad, blob, len);
    bad[0] = 'X';
    fix_hash(bad, len);
    CHECK(lz_load(&other, bad, len) == LZ_E_VALUE);
    {
        /* magic, version, reader, WiFi name and password, then nrelays */
        size_t at = 4 + 1 + 16 + 1 + s->ctx.wifi_name_len + 1 + s->ctx.wifi_password_len;
        memcpy(bad, blob, len);
        CHECK_EQ(bad[at], s->ctx.nrelays);
        bad[at] = LZ_MAX_RELAYS + 1;
        fix_hash(bad, len);
        CHECK(lz_load(&other, bad, len) == LZ_E_LIMIT);
        memcpy(bad, blob, len);
        bad[at] = 0;
        fix_hash(bad, len);
        CHECK(lz_load(&other, bad, len) == LZ_E_LIMIT);
    }
    /* Trailing bytes before the hash. */
    memcpy(bad, blob, len - 32);
    bad[len - 32] = 0;
    memcpy(bad + len - 31, blob + len - 32, 32);
    fix_hash(bad, len + 1);
    CHECK(lz_load(&other, bad, len + 1) == LZ_E_TOO_LONG);
    /* Saving needs an installed state. */
    CHECK(lz_init(&other, &pf) == LZ_OK);
    CHECK(lz_save_buf(&other, again, sizeof again, &len2) == LZ_E_ARG);
}

void suite_store(void) { round_trip_and_damage(); }
