/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * crypto_box and crypto_secretbox in libsodium's _easy layout, on
 * TweetNaCl or libsodium.
 */
#include <string.h>

#include "lz/crypto.h"

int lz_ct_equal(const uint8_t *a, const uint8_t *b, size_t len)
{
    volatile uint8_t d = 0;
    size_t i;
    for (i = 0; i < len; i++) d |= (uint8_t)(a[i] ^ b[i]);
    return d == 0;
}

void lz_wipe(void *p, size_t len)
{
    volatile uint8_t *v = (volatile uint8_t *)p;
    while (len--) *v++ = 0;
}

#if defined(LZ_CRYPTO_LIBSODIUM)

#include <sodium.h>

int lz_crypto_init(void)
{
    if (sodium_init() < 0) return -1;
#if defined(LZ_SHA256_MBEDTLS)
    {
        uint8_t h[32];
        lz_sha256(h, (const uint8_t *)"", 0); /* runs psa_crypto_init */
    }
#endif
    return 0;
}

int lz_scalarmult_base(uint8_t pk[32], const uint8_t sk[32])
{
    return crypto_scalarmult_base(pk, sk) == 0 ? 0 : -1;
}

int lz_box_beforenm(uint8_t k[32], const uint8_t pk[32], const uint8_t sk[32])
{
    /* libsodium refuses low-order points itself. */
    if (crypto_box_beforenm(k, pk, sk) != 0) {
        lz_wipe(k, 32);
        return -1;
    }
    return 0;
}

int lz_secretbox_easy(uint8_t *c, const uint8_t *m, size_t mlen, const uint8_t n[24],
                      const uint8_t k[32])
{
    if (mlen > LZ_CRYPTO_MAX_MSG) return -1;
    return crypto_secretbox_easy(c, m, (unsigned long long)mlen, n, k) == 0 ? 0 : -1;
}

int lz_secretbox_open_easy(uint8_t *m, const uint8_t *c, size_t clen, const uint8_t n[24],
                           const uint8_t k[32])
{
    if (clen < LZ_TAG_LEN || clen - LZ_TAG_LEN > LZ_CRYPTO_MAX_MSG) return -1;
    if (crypto_secretbox_open_easy(m, c, (unsigned long long)clen, n, k) != 0) {
        memset(m, 0, clen - LZ_TAG_LEN);
        return -1;
    }
    return 0;
}

#else /* TweetNaCl */

#include "tweetnacl.h"

int lz_crypto_init(void)
{
#if defined(LZ_SHA256_MBEDTLS)
    uint8_t h[32];
    lz_sha256(h, (const uint8_t *)"", 0); /* runs psa_crypto_init */
#endif
    return 0;
}

int lz_scalarmult_base(uint8_t pk[32], const uint8_t sk[32])
{
    return lz_tweetnacl_scalarmult_base(pk, sk);
}

int lz_box_beforenm(uint8_t k[32], const uint8_t pk[32], const uint8_t sk[32])
{
    uint8_t s[32];
    static const uint8_t zero[32] = {0};
    /* TweetNaCl does not refuse low-order points; check the shared
     * secret as libsodium does, so that both backends agree. */
    lz_tweetnacl_scalarmult(s, sk, pk);
    if (lz_tweetnacl_verify_32(s, zero) == 0) {
        lz_wipe(s, sizeof s);
        lz_wipe(k, 32);
        return -1;
    }
    lz_wipe(s, sizeof s);
    lz_tweetnacl_box_beforenm(k, pk, sk);
    return 0;
}

/* TweetNaCl wants 32 zero bytes before the message and gives 16 zero bytes
 * before the tag; libsodium's _easy layout is tag || ciphertext. */
int lz_secretbox_easy(uint8_t *c, const uint8_t *m, size_t mlen, const uint8_t n[24],
                      const uint8_t k[32])
{
    uint8_t pm[32 + LZ_CRYPTO_MAX_MSG];
    uint8_t pc[32 + LZ_CRYPTO_MAX_MSG];
    if (mlen > LZ_CRYPTO_MAX_MSG) return -1;
    memset(pm, 0, 32);
    memcpy(pm + 32, m, mlen);
    if (lz_tweetnacl_secretbox(pc, pm, (uint64_t)(32 + mlen), n, k) != 0) {
        lz_wipe(pm, sizeof pm);
        return -1;
    }
    memcpy(c, pc + 16, 16 + mlen);
    lz_wipe(pm, sizeof pm);
    lz_wipe(pc, sizeof pc);
    return 0;
}

int lz_secretbox_open_easy(uint8_t *m, const uint8_t *c, size_t clen, const uint8_t n[24],
                           const uint8_t k[32])
{
    uint8_t pc[32 + LZ_CRYPTO_MAX_MSG];
    uint8_t pm[32 + LZ_CRYPTO_MAX_MSG];
    size_t mlen;
    if (clen < LZ_TAG_LEN || clen - LZ_TAG_LEN > LZ_CRYPTO_MAX_MSG) return -1;
    mlen = clen - LZ_TAG_LEN;
    memset(pc, 0, 16);
    memcpy(pc + 16, c, clen);
    if (lz_tweetnacl_secretbox_open(pm, pc, (uint64_t)(32 + mlen), n, k) != 0) {
        memset(m, 0, mlen);
        lz_wipe(pc, sizeof pc);
        return -1;
    }
    memcpy(m, pm + 32, mlen);
    lz_wipe(pm, sizeof pm);
    lz_wipe(pc, sizeof pc);
    return 0;
}

#endif
