/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The ESP-IDF adapter around lz_core: one task owns the context and moves
 * bytes between it and WiFi, NTP, the relays (HTTPS and WebSocket with the
 * certificate bundle) and NVS. The application supplies the board: display,
 * light and buttons, through hooks that run in that task.
 *
 *   nvs_flash_init();
 *   if (!lz_esp_has_state()) {           // fresh or factory reset
 *       lz_esp_setup_serial(...);         // setup over USB serial
 *   }
 *   lz_esp_start(&hooks);
 *   // from button handlers, any task:
 *   lz_esp_press(); lz_esp_long_press(); lz_esp_ask(1);
 */
#ifndef LZ_ESP_H
#define LZ_ESP_H

#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "lz/lz.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Connection status for the display. */
typedef struct {
    uint8_t wifi;       /* associated with an address */
    uint8_t time;       /* the clock is set (NTP) */
    uint8_t relays;     /* relays the setup uses */
    uint8_t connected;  /* of them with an open WebSocket */
} lz_esp_status;

/* Called in the Lebenszeichen task. ctx is valid only during the call and
 * must not be changed. */
typedef struct {
    void *user;
    void (*view)(void *user, const lz_ctx *ctx);  /* the display changed */
    void (*led)(void *user, int led);              /* LZ_LED_* */
    void (*event)(void *user, const lz_ctx *ctx, const lz_event *ev);
    void (*status)(void *user, const lz_esp_status *st);
    void (*tick)(void *user, const lz_ctx *ctx);   /* once a second */
    /* Removed on a full device or factory reset: the state is erased.
     * The adapter restarts the chip after this returns. */
    void (*erased)(void *user);
} lz_esp_hooks;

/* Whether NVS holds a state. Needs nvs_flash_init(). */
int lz_esp_has_state(void);

/* Installs a setup (wire-format.md, "Satellite setup") and stores it.
 * Before lz_esp_start. LZ_OK or the core's error. */
int lz_esp_install(const uint8_t *setup, size_t len);

/* Erases the stored state (factory reset). */
esp_err_t lz_esp_erase(void);

/* Loads the state, starts WiFi, NTP and the task. ESP_ERR_NOT_FOUND
 * without a stored state, ESP_ERR_INVALID_STATE if it does not load. */
esp_err_t lz_esp_start(const lz_esp_hooks *hooks);

/* Buttons; safe from any task, also before the task runs (dropped then). */
void lz_esp_press(void);
void lz_esp_long_press(void);
void lz_esp_ask(uint8_t button);

/* Setup over USB serial (USB-Serial-JTAG), for development until BLE:
 * reads the hex that `lz satellite add` prints, line by line, and returns
 * as soon as it forms a setup the core accepts; that setup is installed
 * and stored. Logging is silenced while it waits. progress(user, bytes,
 * refused) is called as hex arrives, and with bytes 0 when input stopped
 * for 5 seconds without forming a setup (refused: the core's error for
 * what arrived, LZ_OK if nothing did); it may be NULL. Returns LZ_OK once
 * installed; blocks until then. */
int lz_esp_setup_serial(void (*progress)(void *user, size_t bytes, int refused), void *user);

#ifdef __cplusplus
}
#endif

#endif
