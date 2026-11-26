/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Deterministic fuzzing of every parser: random and mutated inputs must
 * never read outside their buffer (run under ASan and UBSan in CI), and
 * whatever is accepted must be consistent.
 */
#include <stdlib.h>

#include "scen.h"

void store_rich_state(scen *s);

static t_rng R;

static uint32_t rnd(uint32_t n)
{
    uint8_t b[4];
    t_rng_fill(&R, b, 4);
    return n ? (((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3]) % n : 0;
}

static void mutate(uint8_t *buf, size_t *len, size_t cap)
{
    int k, n = 1 + (int)rnd(4);
    for (k = 0; k < n; k++) {
        switch (rnd(5)) {
        case 0:
            if (*len) buf[rnd((uint32_t)*len)] ^= (uint8_t)(1u << rnd(8));
            break;
        case 1:
            if (*len) buf[rnd((uint32_t)*len)] = (uint8_t)rnd(256);
            break;
        case 2:
            if (*len) *len = rnd((uint32_t)*len + 1);
            break;
        case 3:
            if (*len < cap) buf[(*len)++] = (uint8_t)rnd(256);
            break;
        default:
            if (*len) buf[rnd((uint32_t)*len)] = (uint8_t)(rnd(2) ? 0 : 0xFF);
            break;
        }
    }
}

static void messages(void)
{
    uint8_t p[LZ_PLAINTEXT_LEN], q[LZ_PLAINTEXT_LEN];
    lz_message m, m2;
    int i;
    for (i = 0; i < 20000; i++) {
        size_t len = LZ_PLAINTEXT_LEN;
        t_rng_fill(&R, p, sizeof p);
        /* Mostly known types and plausible lengths, to reach the bodies. */
        if (rnd(4)) {
            static const uint8_t types[] = {3, 4, 5, 6, 80, 81, 82, 83, 88, 89, 90, 91, 92, 93, 94, 95, 96, 1, 7, 64, 97, 255};
            p[0] = types[rnd(sizeof types)];
            p[25] = 0;
            p[26] = (uint8_t)rnd(230);
        }
        if (rnd(3) == 0) len = rnd(LZ_PLAINTEXT_LEN + 1);
        if (lz_message_decode(&m, p, len) == LZ_OK && lz_type_known(m.type)) {
            CHECK(lz_message_encode(&m, q) == LZ_OK);
            CHECK(lz_message_decode(&m2, q, sizeof q) == LZ_OK);
            CHECK_EQ(m2.type, m.type);
        }
    }
}

static void records_and_setups(void)
{
    scen *s = &S;
    uint8_t setup[4096], buf[4096], rec[LZ_RECORD_MAX_LEN + 16];
    size_t slen, rlen, len;
    lz_contact_record r;
    lz_setup_header h;
    lz_platform pf;
    static lz_ctx ctx;
    int i;
    scen_init(s, 2, NULL);
    slen = scen_setup(s, setup, sizeof setup, NULL);
    scen_record(s, 0, &r);
    rlen = lz_contact_record_encode(&r, rec);
    t_platform(&pf, &s->w);
    CHECK(lz_init(&ctx, &pf) == LZ_OK);
    for (i = 0; i < 3000; i++) {
        len = rlen;
        memcpy(buf, rec, rlen);
        mutate(buf, &len, sizeof buf);
        if (lz_contact_record_decode(&r, buf, len) == LZ_OK) {
            uint8_t again[LZ_RECORD_MAX_LEN];
            lz_contact_record r2;
            size_t n = lz_contact_record_encode(&r, again);
            CHECK(n > 0);
            CHECK(lz_contact_record_decode(&r2, again, n) == LZ_OK);
        }
        len = slen;
        memcpy(buf, setup, slen);
        mutate(buf, &len, sizeof buf);
        if (lz_setup_header_decode(&h, buf, len) == LZ_OK) CHECK(h.entries_offset <= len);
        /* Installing runs X25519 per contact: every tenth input. */
        if (i % 10 == 0 && lz_install(&ctx, buf, len) == LZ_OK) {
            CHECK(ctx.ncontacts <= LZ_MAX_CONTACTS && ctx.nrelays <= LZ_MAX_RELAYS);
        }
    }
}

static void frames(void)
{
    static const char *seeds[] = {
        "{\"op\":\"message\",\"mailbox\":\"QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI\",\"seq\":3,\"data\":\"AA\"}",
        "{\"op\":\"error\",\"mailbox\":\"QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI\",\"code\":\"unknown_reader\"}",
        "{\"op\":\"subscribed\",\"mailbox\":\"QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI\",\"x\":[{\"a\":\"\\u00e4\"},1e5,null]}",
        "{\"features\":[\"reader_tokens\"]}",
        "{\"error\":\"rate_limited\"}",
    };
    uint8_t buf[600];
    char code[LZ_MAX_ERROR_CODE + 1];
    lz_frame f;
    int i;
    for (i = 0; i < 30000; i++) {
        const char *seed = seeds[rnd(sizeof seeds / sizeof seeds[0])];
        size_t len = strlen(seed);
        memcpy(buf, seed, len);
        mutate(buf, &len, sizeof buf);
        if (lz_frame_parse(&f, (const char *)buf, len) == LZ_OK && f.op == LZ_FRAME_ERROR) {
            CHECK(strlen(f.code) > 0 && strlen(f.code) <= LZ_MAX_ERROR_CODE);
        }
        lz_parse_error_body(code, (const char *)buf, len);
        CHECK(strlen(code) <= LZ_MAX_ERROR_CODE);
        (void)lz_health_has_reader_tokens((const char *)buf, len);
    }
}

/* Mutated envelopes and frames into a running satellite. */
static void the_core(void)
{
    scen *s = &S;
    uint8_t env[LZ_ENVELOPE_LEN];
    int i;
    scen_init(s, 2, NULL);
    for (i = 0; i < 400; i++) {
        lz_message m;
        size_t len = LZ_ENVELOPE_LEN;
        int to_channel = (int)rnd(2);
        uint8_t plain[LZ_PLAINTEXT_LEN], nonce[24];
        memset(&m, 0, sizeof m);
        /* A valid plaintext with a mutated body, sealed properly. */
        t_rng_fill(&R, plain, sizeof plain);
        plain[0] = (uint8_t)(to_channel ? 88 + rnd(9) : 3 + rnd(4));
        lz_put_u64(plain + 17, s->w.now - rnd(100));
        plain[25] = 0;
        plain[26] = (uint8_t)rnd(100);
        if (rnd(2)) plain[27 + 48] = (uint8_t)rnd(3); /* plausible kinds */
        if (to_channel && rnd(2)) memcpy(plain + 27, s->c[rnd(2)].own_mailbox, 32);
        t_rng_fill(&R, nonce, 24);
        if (to_channel) {
            lz_seal_plain(env, plain, s->key, nonce);
            if (rnd(4) == 0) mutate(env, &len, sizeof env);
            fr_put(&s->fr, s->mailbox, env);
        } else {
            plain[255] = (uint8_t)rnd(3);
            lz_seal_plain(env, plain, s->c[0].k, nonce);
            if (rnd(4) == 0) mutate(env, &len, sizeof env);
            fr_put(&s->fr, s->c[0].own_mailbox, env);
        }
        fr_run(&s->fr);
        if (!s->ctx.installed) break;
        if (rnd(10) == 0) {
            lz_press(&s->ctx);
            fr_run(&s->fr);
        }
        if (rnd(10) == 0) scen_advance(s, rnd(3600));
    }
    /* Raw frames of every shape into the core. */
    for (i = 0; i < 3000 && s->ctx.installed; i++) {
        char frame[700];
        uint8_t buf[700];
        size_t len;
        char mb[64], data[400];
        lz_b64url_encode(mb, sizeof mb, s->c[rnd(2)].own_mailbox, 32);
        t_rng_fill(&R, env, sizeof env);
        lz_b64url_encode(data, sizeof data, env, sizeof env);
        snprintf(frame, sizeof frame, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":%u,\"data\":\"%s\"}",
                 mb, (unsigned)rnd(100000), data);
        len = strlen(frame);
        memcpy(buf, frame, len);
        if (rnd(2)) mutate(buf, &len, sizeof buf);
        lz_received(&s->ctx, 0, (const char *)buf, len);
        fr_run(&s->fr);
    }
    CHECK(s->ctx.installed);
}

static void blobs(void)
{
    scen *s = &S;
    static uint8_t blob[100000], buf[100000];
    static lz_ctx other;
    size_t len, n;
    lz_platform pf;
    int i;
    store_rich_state(s);
    CHECK(lz_save_buf(&s->ctx, blob, sizeof blob, &len) == LZ_OK);
    t_platform(&pf, &s->w);
    CHECK(lz_init(&other, &pf) == LZ_OK);
    for (i = 0; i < 3000; i++) {
        n = len;
        memcpy(buf, blob, len);
        mutate(buf, &n, sizeof buf);
        if (n >= 32) lz_sha256(buf + n - 32, buf, n - 32); /* past the checksum */
        if (lz_load(&other, buf, n) == LZ_OK) {
            CHECK(other.ncontacts <= LZ_MAX_CONTACTS && other.noutbox <= LZ_OUTBOX_SIZE);
            CHECK(other.channel.relay < other.nrelays);
        }
    }
}

void suite_fuzz(void)
{
    t_rng_init(&R, (const uint8_t *)"fuzz", 4);
    messages();
    records_and_setups();
    frames();
    the_core();
    blobs();
}
