/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The cryptography the satellite needs, in libsodium's layout:
 *
 * - crypto_box (X25519, XSalsa20, Poly1305) for form A, split into
 *   beforenm/afternm so that the slow X25519 step runs once per contact;
 * - crypto_secretbox (XSalsa20, Poly1305) for form C;
 * - SHA-256.
 *
 * The *_easy functions put the 16-byte tag before the ciphertext, as
 * libsodium does and as the envelope of wire-format.md requires.
 *
 * Backends, chosen at build time:
 *   default            TweetNaCl (bundled, third_party/tweetnacl)
 *   LZ_CRYPTO_LIBSODIUM libsodium (ESP-IDF component, Arduino-ESP32, hosts)
 *   SHA-256: bundled by default; LZ_SHA256_MBEDTLS uses mbedTLS through the
 *   PSA API (mbedTLS 3.x and 4.x / TF-PSA-Crypto); with LZ_CRYPTO_LIBSODIUM
 *   and without LZ_SHA256_MBEDTLS, libsodium's.
 */
#ifndef LZ_CRYPTO_H
#define LZ_CRYPTO_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LZ_KEY_LEN 32
#define LZ_NONCE_LEN 24
#define LZ_TAG_LEN 16
#define LZ_SHA256_LEN 32

/* Longest message the *_easy functions take; the protocol needs 256. */
#define LZ_CRYPTO_MAX_MSG 256

/* Initialises the backend (libsodium's sodium_init, PSA's
 * psa_crypto_init). Idempotent. 0 on success. */
int lz_crypto_init(void);

/* Public key for a secret X25519 key. */
int lz_scalarmult_base(uint8_t pk[32], const uint8_t sk[32]);

/* The shared key of crypto_box_beforenm. Fails (-1) for a peer key that
 * gives the all-zero shared secret (a low-order point). */
int lz_box_beforenm(uint8_t k[32], const uint8_t pk[32], const uint8_t sk[32]);

/* c = tag (16) || ciphertext (mlen); mlen <= LZ_CRYPTO_MAX_MSG. c and m
 * may not overlap. */
int lz_secretbox_easy(uint8_t *c, const uint8_t *m, size_t mlen, const uint8_t n[24],
                      const uint8_t k[32]);

/* m = plaintext of c (tag || ciphertext, clen = 16 + mlen). -1 if the tag
 * does not verify; m is then zeroed. */
int lz_secretbox_open_easy(uint8_t *m, const uint8_t *c, size_t clen, const uint8_t n[24],
                           const uint8_t k[32]);

/* crypto_box_easy with a key from lz_box_beforenm: the same as secretbox
 * under that key. */
#define lz_box_easy_afternm lz_secretbox_easy
#define lz_box_open_easy_afternm lz_secretbox_open_easy

/* Incremental SHA-256. The state is large enough for every backend. */
typedef struct {
    uint64_t align_;
    uint8_t opaque[248];
} lz_sha256_state;

void lz_sha256_init(lz_sha256_state *s);
void lz_sha256_update(lz_sha256_state *s, const uint8_t *data, size_t len);
void lz_sha256_final(lz_sha256_state *s, uint8_t out[32]);
void lz_sha256(uint8_t out[32], const uint8_t *data, size_t len);

/* Compares in time independent of the content; 1 if equal. */
int lz_ct_equal(const uint8_t *a, const uint8_t *b, size_t len);

/* Overwrites memory in a way the compiler does not remove. */
void lz_wipe(void *p, size_t len);

#ifdef __cplusplus
}
#endif

#endif
