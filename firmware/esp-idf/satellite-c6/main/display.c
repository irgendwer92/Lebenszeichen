/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <string.h>

#include "display.h"
#include "driver/i2c_master.h"

static i2c_master_dev_handle_t s_dev;
static uint8_t s_fb[1 + DISP_W * DISP_H / 8]; /* control byte, then the pages */
#define FB (s_fb + 1)

static esp_err_t cmds(const uint8_t *c, size_t n)
{
    uint8_t buf[32];
    if (n + 1 > sizeof(buf)) return ESP_ERR_INVALID_SIZE;
    buf[0] = 0x00; /* commands follow */
    memcpy(buf + 1, c, n);
    return i2c_master_transmit(s_dev, buf, n + 1, 100);
}

esp_err_t disp_init(int sda, int scl, uint8_t addr)
{
    i2c_master_bus_config_t bc = {
        .i2c_port = -1,
        .sda_io_num = sda,
        .scl_io_num = scl,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    i2c_master_bus_handle_t bus;
    esp_err_t err = i2c_new_master_bus(&bc, &bus);
    if (err != ESP_OK) return err;
    i2c_device_config_t dc = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = addr,
        .scl_speed_hz = 400000,
    };
    err = i2c_master_bus_add_device(bus, &dc, &s_dev);
    if (err != ESP_OK) return err;
    static const uint8_t init[] = {
        0xAE,       /* off */
        0xD5, 0x80, /* clock */
        0xA8, 0x3F, /* 64 rows */
        0xD3, 0x00, /* no offset */
        0x40,       /* start line 0 */
        0x8D, 0x14, /* charge pump on */
        0x20, 0x00, /* horizontal addressing */
        0xA1, 0xC8, /* not mirrored */
        0xDA, 0x12, /* COM pins */
        0x81, 0x8F, /* contrast */
        0xD9, 0xF1, /* precharge */
        0xDB, 0x40, /* VCOMH */
        0xA4, 0xA6, /* from RAM, not inverted */
    };
    err = cmds(init, sizeof(init));
    if (err != ESP_OK) return err;
    disp_clear();
    err = disp_flush();
    if (err != ESP_OK) return err;
    disp_on(1);
    return ESP_OK;
}

void disp_on(int on)
{
    uint8_t c = on ? 0xAF : 0xAE;
    cmds(&c, 1);
}

void disp_clear(void)
{
    memset(FB, 0, DISP_W * DISP_H / 8);
}

esp_err_t disp_flush(void)
{
    static const uint8_t window[] = {0x21, 0, DISP_W - 1, 0x22, 0, DISP_H / 8 - 1};
    esp_err_t err = cmds(window, sizeof(window));
    if (err != ESP_OK) return err;
    s_fb[0] = 0x40; /* data follows */
    return i2c_master_transmit(s_dev, s_fb, sizeof(s_fb), 200);
}

static void pixel(int x, int y)
{
    if (x < 0 || y < 0 || x >= DISP_W || y >= DISP_H) return;
    FB[(y / 8) * DISP_W + x] |= (uint8_t)(1u << (y & 7));
}

void disp_hline(int x, int y, int w)
{
    for (int i = 0; i < w; i++) pixel(x + i, y);
}

/* The next codepoint of UTF-8; invalid bytes count as U+FFFD. */
static uint32_t next_cp(const char **p, const char *end)
{
    const uint8_t *s = (const uint8_t *)*p;
    uint32_t c = *s++;
    int more = c >= 0xF0 ? 3 : c >= 0xE0 ? 2 : c >= 0xC0 ? 1 : 0;
    if (c >= 0x80 && c < 0xC0) more = -1;
    if (more > 0) {
        c &= 0x3Fu >> more;
        for (int i = 0; i < more; i++) {
            if ((const char *)s >= end || (*s & 0xC0) != 0x80) {
                *p = (const char *)s;
                return 0xFFFD;
            }
            c = c << 6 | (*s++ & 0x3F);
        }
    } else if (more < 0) {
        c = 0xFFFD;
    }
    *p = (const char *)s;
    return c;
}

static const uint8_t *glyph(const lz_font *f, uint32_t cp)
{
    int lo = 0, hi = f->n - 1;
    while (lo <= hi) {
        int mid = (lo + hi) / 2;
        if (f->cps[mid] == cp) return f->bits + (size_t)mid * f->h;
        if (f->cps[mid] < cp) lo = mid + 1;
        else hi = mid - 1;
    }
    return cp == '?' ? NULL : glyph(f, '?');
}

static void draw_glyph(const lz_font *f, int x, int y, uint32_t cp)
{
    const uint8_t *g = glyph(f, cp);
    if (!g) return;
    for (int r = 0; r < f->h; r++)
        for (int c = 0; c < f->w; c++)
            if (g[r] & (0x80 >> c)) pixel(x + c, y + r);
}

static size_t text_len(const char *s, size_t len)
{
    return len == SIZE_MAX ? strlen(s) : len;
}

int disp_text_width(const lz_font *f, const char *s, size_t len)
{
    const char *p = s, *end = s + text_len(s, len);
    int n = 0;
    while (p < end) {
        next_cp(&p, end);
        n++;
    }
    return n * f->w;
}

int disp_text(const lz_font *f, int x, int y, const char *s, size_t len, int max_w)
{
    const char *p = s, *end = s + text_len(s, len);
    int full = disp_text_width(f, s, len) <= max_w;
    int x0 = x;
    while (p < end) {
        uint32_t cp = next_cp(&p, end);
        if (!full && x + 2 * f->w > x0 + max_w) {
            draw_glyph(f, x, y, 0x2026);
            return x + f->w - x0;
        }
        draw_glyph(f, x, y, cp);
        x += f->w;
    }
    return x - x0;
}

void disp_text_center(const lz_font *f, int y, const char *s, size_t len)
{
    int w = disp_text_width(f, s, len);
    if (w > DISP_W) w = DISP_W;
    disp_text(f, (DISP_W - w) / 2, y, s, len, DISP_W);
}
