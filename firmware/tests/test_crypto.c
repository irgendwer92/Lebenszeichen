/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* Known answers for the primitives, and a comparison with libsodium when
 * the tests are built with it (LZ_TEST_SODIUM). */
#include <stdlib.h>

#include "test.h"

#ifdef LZ_TEST_SODIUM
#include <sodium.h>
#endif

static void hex_is(const uint8_t *got, size_t n, const char *hex)
{
    uint8_t want[128];
    CHECK(t_unhex(want, sizeof want, hex, strlen(hex)) == (int)n);
    CHECK_MEM(got, want, n);
}

static void sha256_known_answers(void)
{
    uint8_t h[32];
    lz_sha256_state s;
    static const char *long_msg = "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    size_t i;
    lz_sha256(h, (const uint8_t *)"", 0);
    hex_is(h, 32, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    lz_sha256(h, (const uint8_t *)"abc", 3);
    hex_is(h, 32, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    lz_sha256(h, (const uint8_t *)long_msg, strlen(long_msg));
    hex_is(h, 32, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    /* The same in pieces of every size. */
    for (i = 1; i < strlen(long_msg); i++) {
        size_t off = 0;
        lz_sha256_init(&s);
        while (off < strlen(long_msg)) {
            size_t n = strlen(long_msg) - off < i ? strlen(long_msg) - off : i;
            lz_sha256_update(&s, (const uint8_t *)long_msg + off, n);
            off += n;
        }
        lz_sha256_final(&s, h);
        hex_is(h, 32, "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
    }
    {
        /* One million times 'a'. */
        uint8_t a[1000];
        memset(a, 'a', sizeof a);
        lz_sha256_init(&s);
        for (i = 0; i < 1000; i++) lz_sha256_update(&s, a, sizeof a);
        lz_sha256_final(&s, h);
        hex_is(h, 32, "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }
}

static void x25519_known_answers(void)
{
    uint8_t ask[32], bsk[32], pk[32], k1[32], k2[32];
    t_unhex(ask, 32, "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a", 64);
    t_unhex(bsk, 32, "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb", 64);
    CHECK(lz_scalarmult_base(pk, ask) == 0);
    hex_is(pk, 32, "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    CHECK(lz_scalarmult_base(pk, bsk) == 0);
    hex_is(pk, 32, "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    /* crypto_box_beforenm of NaCl's box test: HSalsa20 of the shared
     * secret, the same from both sides. */
    CHECK(lz_box_beforenm(k1, pk, ask) == 0);
    hex_is(k1, 32, "1b27556473e985d462cd51197a9a46c76009549eac6474f206c4ee0844f68389");
    CHECK(lz_scalarmult_base(pk, ask) == 0);
    CHECK(lz_box_beforenm(k2, pk, bsk) == 0);
    CHECK_MEM(k1, k2, 32);
    /* A low-order peer key gives the all-zero secret and is refused. */
    memset(pk, 0, 32);
    CHECK(lz_box_beforenm(k1, pk, ask) != 0);
    memset(pk, 0, 32);
    pk[0] = 1;
    CHECK(lz_box_beforenm(k1, pk, ask) != 0);
}

static void secretbox_round_trip_and_tamper(void)
{
    uint8_t key[32], nonce[24], m[256], c[16 + 256], back[256];
    size_t i, len;
    for (i = 0; i < 32; i++) key[i] = (uint8_t)(i * 7 + 1);
    for (i = 0; i < 24; i++) nonce[i] = (uint8_t)(i * 3 + 5);
    for (i = 0; i < 256; i++) m[i] = (uint8_t)(i ^ 0x5A);
    for (len = 0; len <= 256; len += 37) {
        CHECK(lz_secretbox_easy(c, m, len, nonce, key) == 0);
        CHECK(lz_secretbox_open_easy(back, c, 16 + len, nonce, key) == 0);
        CHECK_MEM(back, m, len);
    }
    CHECK(lz_secretbox_easy(c, m, 256, nonce, key) == 0);
    for (i = 0; i < 16 + 256; i++) {
        c[i] ^= 0x01;
        CHECK(lz_secretbox_open_easy(back, c, 16 + 256, nonce, key) != 0);
        c[i] ^= 0x01;
    }
    nonce[3] ^= 1;
    CHECK(lz_secretbox_open_easy(back, c, 16 + 256, nonce, key) != 0);
    CHECK(lz_secretbox_easy(c, m, 257, nonce, key) != 0);
    CHECK(lz_secretbox_open_easy(back, c, 15, nonce, key) != 0);
    {
        uint8_t a[4] = {1, 2, 3, 4}, b[4] = {1, 2, 3, 5};
        CHECK(lz_ct_equal(a, a, 4));
        CHECK(!lz_ct_equal(a, b, 4));
    }
}

#ifdef LZ_TEST_SODIUM
static void against_libsodium(void)
{
    t_rng rng;
    int round;
    CHECK(sodium_init() >= 0);
    t_rng_init(&rng, (const uint8_t *)"sodium", 6);
    for (round = 0; round < 64; round++) {
        uint8_t sk[32], sk2[32], pk[32], pk2[32], spk[32], k[32], sk_k[32], nonce[24];
        uint8_t m[256], c[272], sc[272], h[32], sh[32];
        size_t len;
        t_rng_fill(&rng, sk, 32);
        t_rng_fill(&rng, sk2, 32);
        t_rng_fill(&rng, nonce, 24);
        t_rng_fill(&rng, m, sizeof m);
        len = (size_t)(m[0]) + (round == 0 ? 1 : 0);
        if (len > 256) len = 256;
        CHECK(lz_scalarmult_base(pk, sk) == 0);
        crypto_scalarmult_base(spk, sk);
        CHECK_MEM(pk, spk, 32);
        crypto_scalarmult_base(pk2, sk2);
        CHECK(lz_box_beforenm(k, pk2, sk) == 0);
        CHECK(crypto_box_beforenm(sk_k, pk2, sk) == 0);
        CHECK_MEM(k, sk_k, 32);
        CHECK(lz_box_easy_afternm(c, m, len, nonce, k) == 0);
        CHECK(crypto_box_easy(sc, m, len, nonce, pk2, sk) == 0);
        CHECK_MEM(c, sc, 16 + len);
        CHECK(crypto_box_open_easy(sc, c, 16 + len, nonce, pk, sk2) == 0);
        CHECK_MEM(sc, m, len);
        CHECK(lz_secretbox_easy(c, m, len, nonce, k) == 0);
        CHECK(crypto_secretbox_easy(sc, m, len, nonce, k) == 0);
        CHECK_MEM(c, sc, 16 + len);
        lz_sha256(h, m, len);
        crypto_hash_sha256(sh, m, len);
        CHECK_MEM(h, sh, 32);
    }
}
#endif

void suite_crypto(void)
{
    CHECK(lz_crypto_init() == 0);
    sha256_known_answers();
    x25519_known_answers();
    secretbox_round_trip_and_tamper();
#ifdef LZ_TEST_SODIUM
    against_libsodium();
#endif
}
