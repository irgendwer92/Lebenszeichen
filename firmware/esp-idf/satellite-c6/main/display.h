/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* SSD1306 128x64 over I2C, a framebuffer and UTF-8 text. */
#ifndef SAT_DISPLAY_H
#define SAT_DISPLAY_H

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "font.h"

#define DISP_W 128
#define DISP_H 64

esp_err_t disp_init(int sda, int scl, uint8_t addr);
void disp_clear(void);
void disp_on(int on);
esp_err_t disp_flush(void);
void disp_hline(int x, int y, int w);
/* Draws UTF-8 text (len bytes, or up to NUL with len SIZE_MAX), at most
 * max_w pixels wide; a cut text ends in an ellipsis. Returns the width. */
int disp_text(const lz_font *f, int x, int y, const char *s, size_t len, int max_w);
/* Width of the text as disp_text would draw it without a limit. */
int disp_text_width(const lz_font *f, const char *s, size_t len);
/* Centered horizontally. */
void disp_text_center(const lz_font *f, int y, const char *s, size_t len);

#endif
