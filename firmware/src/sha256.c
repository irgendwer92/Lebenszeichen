/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * SHA-256 (FIPS 180-4). The bundled implementation, for hosts and SDKs
 * without one; with LZ_SHA256_MBEDTLS mbedTLS's PSA API is used instead,
 * with LZ_CRYPTO_LIBSODIUM libsodium's.
 */
#include <string.h>

#include "lz/crypto.h"

#if defined(LZ_SHA256_MBEDTLS)

#include <psa/crypto.h>

typedef struct {
    psa_hash_operation_t op;
} psa_state;

typedef char psa_state_fits[(sizeof(psa_state) <= sizeof(((lz_sha256_state *)0)->opaque)) ? 1 : -1];

void lz_sha256_init(lz_sha256_state *s)
{
    psa_state *p = (psa_state *)(void *)s->opaque;
    (void)psa_crypto_init();
    p->op = psa_hash_operation_init();
    (void)psa_hash_setup(&p->op, PSA_ALG_SHA_256);
}

void lz_sha256_update(lz_sha256_state *s, const uint8_t *data, size_t len)
{
    psa_state *p = (psa_state *)(void *)s->opaque;
    (void)psa_hash_update(&p->op, data, len);
}

void lz_sha256_final(lz_sha256_state *s, uint8_t out[32])
{
    psa_state *p = (psa_state *)(void *)s->opaque;
    size_t len = 0;
    if (psa_hash_finish(&p->op, out, 32, &len) != PSA_SUCCESS || len != 32) {
        /* Never use a partial hash: fail closed with a value no token or
         * key matches by chance. */
        memset(out, 0xFF, 32);
        (void)psa_hash_abort(&p->op);
    }
    lz_wipe(s, sizeof *s);
}

#elif defined(LZ_CRYPTO_LIBSODIUM)

#include <sodium.h>

typedef char sodium_state_fits
    [(sizeof(crypto_hash_sha256_state) <= sizeof(((lz_sha256_state *)0)->opaque)) ? 1 : -1];

void lz_sha256_init(lz_sha256_state *s)
{
    crypto_hash_sha256_init((crypto_hash_sha256_state *)(void *)s->opaque);
}

void lz_sha256_update(lz_sha256_state *s, const uint8_t *data, size_t len)
{
    crypto_hash_sha256_update((crypto_hash_sha256_state *)(void *)s->opaque, data, len);
}

void lz_sha256_final(lz_sha256_state *s, uint8_t out[32])
{
    crypto_hash_sha256_final((crypto_hash_sha256_state *)(void *)s->opaque, out);
    lz_wipe(s, sizeof *s);
}

#else

typedef struct {
    uint32_t h[8];
    uint64_t total;
    uint8_t buf[64];
    size_t used;
} bundled_state;

typedef char bundled_state_fits
    [(sizeof(bundled_state) <= sizeof(((lz_sha256_state *)0)->opaque)) ? 1 : -1];

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
    0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
    0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
    0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
    0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
    0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2};

static uint32_t rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static void block(uint32_t h[8], const uint8_t p[64])
{
    uint32_t w[64], a, b, c, d, e, f, g, k, t1, t2;
    int i;
    for (i = 0; i < 16; i++) {
        w[i] = ((uint32_t)p[4 * i] << 24) | ((uint32_t)p[4 * i + 1] << 16) |
               ((uint32_t)p[4 * i + 2] << 8) | (uint32_t)p[4 * i + 3];
    }
    for (i = 16; i < 64; i++) {
        uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    a = h[0];
    b = h[1];
    c = h[2];
    d = h[3];
    e = h[4];
    f = h[5];
    g = h[6];
    k = h[7];
    for (i = 0; i < 64; i++) {
        t1 = k + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[i] + w[i];
        t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        k = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += k;
    lz_wipe(w, sizeof w);
}

void lz_sha256_init(lz_sha256_state *s)
{
    static const uint32_t iv[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                                   0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    bundled_state *b = (bundled_state *)(void *)s->opaque;
    memcpy(b->h, iv, sizeof iv);
    b->total = 0;
    b->used = 0;
}

void lz_sha256_update(lz_sha256_state *s, const uint8_t *data, size_t len)
{
    bundled_state *b = (bundled_state *)(void *)s->opaque;
    b->total += (uint64_t)len;
    while (len > 0) {
        size_t take = 64 - b->used;
        if (take > len) take = len;
        memcpy(b->buf + b->used, data, take);
        b->used += take;
        data += take;
        len -= take;
        if (b->used == 64) {
            block(b->h, b->buf);
            b->used = 0;
        }
    }
}

void lz_sha256_final(lz_sha256_state *s, uint8_t out[32])
{
    bundled_state *b = (bundled_state *)(void *)s->opaque;
    uint64_t bits = b->total * 8;
    int i;
    b->buf[b->used++] = 0x80;
    if (b->used > 56) {
        memset(b->buf + b->used, 0, 64 - b->used);
        block(b->h, b->buf);
        b->used = 0;
    }
    memset(b->buf + b->used, 0, 56 - b->used);
    for (i = 0; i < 8; i++) b->buf[56 + i] = (uint8_t)(bits >> (56 - 8 * i));
    block(b->h, b->buf);
    for (i = 0; i < 8; i++) {
        out[4 * i] = (uint8_t)(b->h[i] >> 24);
        out[4 * i + 1] = (uint8_t)(b->h[i] >> 16);
        out[4 * i + 2] = (uint8_t)(b->h[i] >> 8);
        out[4 * i + 3] = (uint8_t)b->h[i];
    }
    lz_wipe(s, sizeof *s);
}

#endif

void lz_sha256(uint8_t out[32], const uint8_t *data, size_t len)
{
    lz_sha256_state s;
    lz_sha256_init(&s);
    lz_sha256_update(&s, data, len);
    lz_sha256_final(&s, out);
}
