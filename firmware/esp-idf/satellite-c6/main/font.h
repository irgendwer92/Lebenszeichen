/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#ifndef SAT_FONT_H
#define SAT_FONT_H

#include <stdint.h>

/* A fixed-width bitmap font: one byte per row, MSB left, sorted codepoints. */
typedef struct {
    uint8_t w, h;
    uint16_t n;
    const uint16_t *cps;
    const uint8_t *bits;
} lz_font;

extern const lz_font font_small; /* 6x10 */
extern const lz_font font_large; /* 7x14 bold */

#endif
