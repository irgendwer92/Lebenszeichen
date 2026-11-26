/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* What the OLED and the light show. Called from the Lebenszeichen task
 * (hooks), or from app_main before it starts. */
#ifndef SAT_UI_H
#define SAT_UI_H

#include "lz_esp.h"

void ui_message(const char *big, const char *small);
void ui_setup(size_t bytes, int refused);
void ui_hooks(lz_esp_hooks *h);
/* From the button task: switch the display on. */
void ui_wake(void);

#endif
