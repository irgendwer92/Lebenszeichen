/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Setup over USB serial (development, until BLE setup, m10-satellite.md
 * decision 10): `lz satellite add` prints the setup as hex, 64 digits per
 * line. Every line end tries what has arrived so far; the first setup the
 * core accepts is installed. Nothing is printed while waiting, so that a
 * host terminal that echoes cannot feed log lines back in.
 */
#include <stdlib.h>
#include <string.h>

#include "driver/usb_serial_jtag.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"

#include "lz_esp.h"

/* The largest setup: header and every contact at its largest. */
#define SETUP_MAX (461 + LZ_MAX_CONTACTS * (32 + 2 + 698))
#define IDLE_MS 5000

static int hexval(uint8_t c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static void say(const char *s)
{
    usb_serial_jtag_write_bytes(s, strlen(s), pdMS_TO_TICKS(100));
}

int lz_esp_setup_serial(void (*progress)(void *user, size_t bytes, int refused), void *user)
{
    usb_serial_jtag_driver_config_t cfg = {.rx_buffer_size = 1024, .tx_buffer_size = 256};
    if (usb_serial_jtag_driver_install(&cfg) != ESP_OK) return LZ_E_STATE;
    uint8_t *buf = malloc(SETUP_MAX);
    if (!buf) {
        usb_serial_jtag_driver_uninstall();
        return LZ_E_FULL;
    }
    esp_log_level_t level = esp_log_level_get("*");
    esp_log_level_set("*", ESP_LOG_NONE);

    size_t n = 0;
    int nibble = -1, last = LZ_OK, done = 0;
    TickType_t quiet_since = xTaskGetTickCount();
    uint8_t chunk[128];
    while (!done) {
        int got = usb_serial_jtag_read_bytes(chunk, sizeof(chunk), pdMS_TO_TICKS(250));
        if (got <= 0) {
            if ((n || last != LZ_OK) &&
                xTaskGetTickCount() - quiet_since > pdMS_TO_TICKS(IDLE_MS)) {
                /* Input stopped without a setup: start over. */
                if (progress) progress(user, 0, last);
                lz_wipe(buf, n);
                n = 0;
                nibble = -1;
                last = LZ_OK;
            }
            continue;
        }
        quiet_since = xTaskGetTickCount();
        for (int i = 0; i < got && !done; i++) {
            uint8_t c = chunk[i];
            int v = hexval(c);
            if (v >= 0) {
                if (nibble < 0) {
                    nibble = v;
                } else if (n < SETUP_MAX) {
                    buf[n++] = (uint8_t)(nibble << 4 | v);
                    nibble = -1;
                } else {
                    last = LZ_E_TOO_LONG;
                }
            } else if (c == '\n' || c == '\r') {
                if (n && nibble < 0) {
                    last = lz_esp_install(buf, n);
                    if (last == LZ_OK) done = 1;
                    else if (progress) progress(user, n, LZ_OK);
                }
            } else if (c != ' ' && c != '\t') {
                /* Anything else is not a setup: drop what came so far. */
                lz_wipe(buf, n);
                n = 0;
                nibble = -1;
            }
        }
    }
    lz_wipe(buf, SETUP_MAX);
    lz_wipe(chunk, sizeof(chunk));
    free(buf);
    say("lz-setup: ok\n");
    vTaskDelay(pdMS_TO_TICKS(100));
    esp_log_level_set("*", level);
    usb_serial_jtag_driver_uninstall();
    return LZ_OK;
}
