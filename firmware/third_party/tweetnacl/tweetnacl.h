/*
 * TweetNaCl, the parts lz_core uses. Public domain; see README.md in this
 * directory.
 */
#ifndef LZ_TWEETNACL_H
#define LZ_TWEETNACL_H

#include <stdint.h>

/* Zero-padded NaCl layout: m starts with 32 zero bytes, c with 16. */
int lz_tweetnacl_secretbox(uint8_t *c, const uint8_t *m, uint64_t d, const uint8_t *n,
                           const uint8_t *k);
int lz_tweetnacl_secretbox_open(uint8_t *m, const uint8_t *c, uint64_t d, const uint8_t *n,
                                const uint8_t *k);
int lz_tweetnacl_scalarmult(uint8_t *q, const uint8_t *n, const uint8_t *p);
int lz_tweetnacl_scalarmult_base(uint8_t *q, const uint8_t *n);
int lz_tweetnacl_box_beforenm(uint8_t *k, const uint8_t *y, const uint8_t *x);
int lz_tweetnacl_verify_32(const uint8_t *x, const uint8_t *y);

#endif
