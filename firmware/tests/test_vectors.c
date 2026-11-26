/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Reproduces the Rust test vectors (crates/client/src/tests/reference.rs,
 * vectors()) byte for byte: the vector randomness is replayed, so every
 * envelope is sealed again here and must come out identical; every
 * plaintext is decoded and encoded again; the record and the setup are
 * parsed and re-encoded; the rule cases are run on the installed setup.
 */
#include <stdlib.h>

#include "test.h"

static size_t hex_field(const t_json *o, const char *key, uint8_t *out, size_t cap)
{
    size_t len;
    const char *s = t_json_str(t_json_get(o, key), &len);
    int n = t_unhex(out, cap, s, len);
    CHECK(n >= 0);
    return n < 0 ? 0 : (size_t)n;
}

static void check_plaintext_round_trip(const uint8_t plain[LZ_PLAINTEXT_LEN], int form_a)
{
    lz_message m;
    uint8_t again[LZ_PLAINTEXT_LEN];
    CHECK(lz_message_decode(&m, plain, LZ_PLAINTEXT_LEN) == LZ_OK);
    CHECK(lz_message_encode(&m, again) == LZ_OK);
    /* Form A carries the direction marker in the last byte. */
    CHECK_MEM(again, plain, form_a ? LZ_PLAINTEXT_LEN - 1 : LZ_PLAINTEXT_LEN);
}

static int g_channel, g_contact;

static void channel_vectors(const t_json *root, t_rng *rng, const uint8_t key[32])
{
    const t_json *list = t_json_get(root, "channel");
    size_t i;
    CHECK(t_json_len(list) >= 15);
    for (i = 0; i < t_json_len(list); i++) {
        const t_json *v = t_json_at(list, i);
        uint8_t plain[LZ_PLAINTEXT_LEN], env[LZ_ENVELOPE_LEN], mine[LZ_ENVELOPE_LEN];
        uint8_t id[16], nonce[24], opened[LZ_PLAINTEXT_LEN];
        size_t dlen;
        const char *dir = t_json_str(t_json_get(v, "direction"), &dlen);
        lz_message m;
        int type = (int)t_json_num(t_json_get(v, "type"));
        CHECK(hex_field(v, "plaintext", plain, sizeof plain) == LZ_PLAINTEXT_LEN);
        CHECK(hex_field(v, "envelope", env, sizeof env) == LZ_ENVELOPE_LEN);
        /* The identifier and then the nonce come from the vector RNG. */
        t_rng_fill(rng, id, 16);
        t_rng_fill(rng, nonce, 24);
        CHECK_MEM(plain + 1, id, 16);
        CHECK(lz_seal_plain(mine, plain, key, nonce) == LZ_OK);
        CHECK_MEM(mine, env, LZ_ENVELOPE_LEN);
        CHECK(lz_open_plain(opened, env, LZ_ENVELOPE_LEN, key) == LZ_OK);
        CHECK_MEM(opened, plain, LZ_PLAINTEXT_LEN);
        CHECK(lz_open_c(&m, env, LZ_ENVELOPE_LEN, key) == LZ_OK);
        CHECK_EQ(m.type, type);
        CHECK(lz_type_known(m.type));
        if (dlen == 2 && memcmp(dir, "up", 2) == 0) {
            CHECK(lz_type_is_up(m.type));
        } else {
            CHECK(lz_type_is_down(m.type));
        }
        CHECK_EQ(m.sent_at, 1794600000);
        check_plaintext_round_trip(plain, 0);
        /* Sealing the decoded message again gives the same envelope. */
        CHECK(lz_seal_c(mine, &m, key, nonce) == LZ_OK);
        CHECK_MEM(mine, env, LZ_ENVELOPE_LEN);
        g_channel++;
    }
}

static void contact_vectors(const t_json *root, t_rng *rng)
{
    const t_json *list = t_json_get(root, "contact");
    uint8_t sat_sk[32], peer_sk[32], sat_pk[32], peer_pk[32], k_sat[32], k_peer[32];
    size_t i;
    memset(sat_sk, 0x31, 32);
    CHECK(hex_field(root, "contact_secret_key", peer_sk, 32) == 32);
    CHECK(lz_scalarmult_base(sat_pk, sat_sk) == 0);
    CHECK(lz_scalarmult_base(peer_pk, peer_sk) == 0);
    CHECK(lz_box_beforenm(k_sat, peer_pk, sat_sk) == 0);
    CHECK(lz_box_beforenm(k_peer, sat_pk, peer_sk) == 0);
    CHECK_MEM(k_sat, k_peer, 32);
    CHECK(t_json_len(list) >= 8);
    for (i = 0; i < t_json_len(list); i++) {
        const t_json *v = t_json_at(list, i);
        uint8_t plain[LZ_PLAINTEXT_LEN], env[LZ_ENVELOPE_LEN], mine[LZ_ENVELOPE_LEN];
        uint8_t id[16], nonce[24];
        size_t flen;
        const char *from = t_json_str(t_json_get(v, "from"), &flen);
        int from_contact = flen == 7 && memcmp(from, "contact", 7) == 0;
        lz_message m;
        CHECK(hex_field(v, "plaintext", plain, sizeof plain) == LZ_PLAINTEXT_LEN);
        CHECK(hex_field(v, "envelope", env, sizeof env) == LZ_ENVELOPE_LEN);
        t_rng_fill(rng, id, 16);
        t_rng_fill(rng, nonce, 24);
        CHECK_MEM(plain + 1, id, 16);
        CHECK(lz_message_decode(&m, plain, LZ_PLAINTEXT_LEN) == LZ_OK);
        CHECK_EQ(m.type, t_json_num(t_json_get(v, "type")));
        check_plaintext_round_trip(plain, 1);
        if (from_contact) {
            lz_message got;
            /* The satellite opens what the contact sealed... */
            CHECK(lz_open_a(&got, env, LZ_ENVELOPE_LEN, k_sat, sat_pk, peer_pk) == LZ_OK);
            CHECK_EQ(got.type, m.type);
            /* ...and the contact's own copy, reflected to it, does not. */
            CHECK(lz_open_a(&got, env, LZ_ENVELOPE_LEN, k_peer, peer_pk, sat_pk) == LZ_E_DECRYPT);
            CHECK(lz_seal_a(mine, &m, k_peer, peer_pk, sat_pk, nonce) == LZ_OK);
        } else {
            lz_message got;
            /* What the satellite seals, byte for byte as Rust does. */
            CHECK(lz_seal_a(mine, &m, k_sat, sat_pk, peer_pk, nonce) == LZ_OK);
            CHECK(lz_open_a(&got, env, LZ_ENVELOPE_LEN, k_peer, peer_pk, sat_pk) == LZ_OK);
            CHECK(lz_open_a(&got, env, LZ_ENVELOPE_LEN, k_sat, sat_pk, peer_pk) == LZ_E_DECRYPT);
        }
        CHECK_MEM(mine, env, LZ_ENVELOPE_LEN);
        g_contact++;
    }
}

static void record_and_setup(const t_json *root, t_world *w)
{
    uint8_t rec_bytes[LZ_RECORD_MAX_LEN + 8], again[LZ_RECORD_MAX_LEN], setup[4096];
    uint8_t mailbox[32];
    size_t rlen = hex_field(root, "satellite_contact_record", rec_bytes, sizeof rec_bytes);
    size_t slen = hex_field(root, "setup", setup, sizeof setup), off;
    lz_contact_record r;
    lz_setup_header h;
    uint8_t key[32];
    static lz_ctx ctx;
    lz_platform pf;
    const t_json *asking = t_json_get(root, "asking"), *rep = t_json_get(root, "reported");
    size_t i;

    CHECK(lz_contact_record_decode(&r, rec_bytes, rlen) == LZ_OK);
    CHECK_EQ(lz_contact_record_encode(&r, again), rlen);
    CHECK_MEM(again, rec_bytes, rlen);
    CHECK_EQ(r.name.len, 3);
    CHECK_MEM(r.name.b, "Mum", 3);
    CHECK_EQ(r.flags.reported, 1);
    CHECK_EQ(r.flags.ask_button, 1);
    CHECK_EQ(r.recent_len, 2);
    CHECK_EQ(r.open_at, 1794600000 - 50);
    CHECK_EQ(r.last_answer, 1794600000 - 7 * 3600);
    /* Every cut of the record fails, trailing bytes are ignored. */
    for (i = 0; i < rlen; i++) CHECK(lz_contact_record_decode(&r, rec_bytes, i) != LZ_OK);
    memset(rec_bytes + rlen, 0xAB, 8);
    CHECK(lz_contact_record_decode(&r, rec_bytes, rlen + 8) == LZ_OK);

    CHECK(lz_setup_header_decode(&h, setup, slen) == LZ_OK);
    CHECK(hex_field(root, "channel_key", key, 32) == 32);
    CHECK_MEM(h.key, key, 32);
    CHECK_EQ(h.contacts, 1);
    CHECK_EQ(h.settings.waiting_minutes, 15);
    CHECK_EQ(h.settings.preset_minutes, 0);
    {
        uint8_t head[LZ_SETUP_HEADER_MAX_LEN];
        size_t hl = t_setup_header(head, sizeof head, &h);
        CHECK_EQ(hl, h.entries_offset);
        CHECK_MEM(head, setup, hl);
    }
    off = h.entries_offset;
    CHECK(lz_setup_entry_next(setup, slen, &off, mailbox, &r) == LZ_OK);
    CHECK_EQ(off, slen);
    {
        uint8_t entry[32 + 2 + LZ_RECORD_MAX_LEN];
        size_t el = t_setup_entry(entry, sizeof entry, mailbox, &r);
        CHECK_MEM(entry, setup + h.entries_offset, el);
    }

    /* Installed, the rule cases of the vector hold. */
    t_platform(&pf, w);
    CHECK(lz_init(&ctx, &pf) == LZ_OK);
    CHECK(lz_install(&ctx, setup, slen) == LZ_OK);
    CHECK_EQ(ctx.ncontacts, 1);
    CHECK(t_json_len(asking) == 4);
    for (i = 0; i < t_json_len(asking); i++) {
        const t_json *c = t_json_at(asking, i);
        uint64_t now = (uint64_t)t_json_num(t_json_get(c, "now"));
        uint64_t want = (uint64_t)t_json_num(t_json_get(c, "next_request_at"));
        CHECK_EQ(lz_next_request_at(&ctx, 0, now), want);
    }
    CHECK_EQ(ctx.settings.waiting_minutes, t_json_num(t_json_get(rep, "waiting_minutes")));
    CHECK_EQ((uint64_t)t_json_num(t_json_get(rep, "request_at")) +
                 (uint64_t)ctx.settings.waiting_minutes * 60u,
             (uint64_t)t_json_num(t_json_get(rep, "due_at")));
}

void suite_vectors(const char *path)
{
    size_t len, slen;
    char *text = t_read_file(path, &len);
    t_json *root;
    t_rng rng;
    uint8_t seed[64], key[32], mine[32];
    t_world w;
    int n;
    CHECK(text != NULL);
    if (!text) return;
    root = t_json_parse(text, len);
    CHECK(root != NULL);
    if (!root) {
        free(text);
        return;
    }
    {
        const char *s = t_json_str(t_json_get(root, "seed"), &slen);
        n = t_unhex(seed, sizeof seed, s, slen);
        CHECK(n > 0);
    }
    t_rng_init(&rng, seed, (size_t)n);
    /* The channel key is the first 32 bytes. */
    t_rng_fill(&rng, mine, 32);
    CHECK(hex_field(root, "channel_key", key, 32) == 32);
    CHECK_MEM(mine, key, 32);
    channel_vectors(root, &rng, key);
    contact_vectors(root, &rng);
    memset(&w, 0, sizeof w);
    w.now = 1794600000;
    t_rng_init(&w.rng, (const uint8_t *)"w", 1);
    record_and_setup(root, &w);
    printf("vectors: %d channel, %d contact envelopes reproduced\n", g_channel, g_contact);
    t_json_free(root);
    free(text);
}
