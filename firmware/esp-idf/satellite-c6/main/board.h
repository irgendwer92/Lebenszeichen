/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* The buttons and the RGB LED. */
#ifndef SAT_BOARD_H
#define SAT_BOARD_H

#include "esp_err.h"

enum {
    LIGHT_OFF = 0,
    LIGHT_REQUEST,  /* blue, slow pulse */
    LIGHT_ANSWER,   /* green, steady dim: the answer to an own ask */
    LIGHT_REPORTED, /* teal, steady dim: a reported answer to an own ask */
    LIGHT_STATE,    /* yellow, steady dim: a state is set */
    LIGHT_WARNING,  /* red, blinking: no WiFi or relay, clock off, access lost */
    LIGHT_SETUP,    /* purple, steady */
    LIGHT_FLASH     /* green, 2 seconds, then back */
};

esp_err_t board_init(void);
void board_light(int light);
/* Whether the answer button is held right now. */
int board_answer_held(void);
/* Starts the button task: short press, long press (1.5 s), ask. on_any is
 * called on every press first (wakes the display). */
void board_buttons_start(void (*on_any)(void));

#endif
