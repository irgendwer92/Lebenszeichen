/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Lebenszeichen satellite on the ESP32-C6 SuperMini (docs/m10-satellite.md):
 * SSD1306 on I2C, answer and ask button to GND, the onboard WS2812.
 *
 * Power-on with the answer button held: a countdown; held for 10 seconds,
 * everything is erased (factory reset). Without a state the satellite
 * waits for a setup over USB serial.
 */
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#include "board.h"
#include "display.h"
#include "esp_log.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lz_esp.h"
#include "nvs_flash.h"
#include "sdkconfig.h"
#include "ui.h"

static const char *TAG = "satellite";

#define RESET_HOLD_S 10

static void halt(const char *big, const char *small)
{
    ui_message(big, small);
    board_light(LIGHT_WARNING);
    for (;;) vTaskDelay(portMAX_DELAY);
}

static void reset_countdown(void)
{
    for (int left = RESET_HOLD_S; left > 0; left--) {
        char line[32];
        snprintf(line, sizeof(line), "Keep holding: %d s", left);
        ui_message("Reset?", line);
        board_light(LIGHT_WARNING);
        for (int i = 0; i < 10; i++) {
            vTaskDelay(pdMS_TO_TICKS(100));
            if (!board_answer_held()) {
                board_light(LIGHT_OFF);
                return;
            }
        }
    }
    if (lz_esp_erase() != ESP_OK) halt("Reset failed", "Erase NVS by flashing");
    ESP_LOGI(TAG, "factory reset");
    ui_message("Reset", "Everything erased");
    board_light(LIGHT_SETUP);
    while (board_answer_held()) vTaskDelay(pdMS_TO_TICKS(100));
    vTaskDelay(pdMS_TO_TICKS(1000));
    esp_restart();
}

static void setup_progress(void *user, size_t bytes, int refused)
{
    (void)user;
    ui_setup(bytes, refused);
}

void app_main(void)
{
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        /* The partition is unusable as it is; the state in it is lost. */
        ESP_ERROR_CHECK(nvs_flash_erase());
        err = nvs_flash_init();
    }
    ESP_ERROR_CHECK(err);

    setenv("TZ", CONFIG_SAT_TZ, 1);
    tzset();

    ESP_ERROR_CHECK(board_init());
    if (disp_init(CONFIG_SAT_PIN_SDA, CONFIG_SAT_PIN_SCL, CONFIG_SAT_OLED_ADDR) != ESP_OK)
        ESP_LOGW(TAG, "no OLED at 0x%02x", CONFIG_SAT_OLED_ADDR);

    if (board_answer_held()) reset_countdown();

    if (!lz_esp_has_state()) {
        ui_setup(0, 0);
        int r = lz_esp_setup_serial(setup_progress, NULL);
        if (r != LZ_OK) halt("Setup failed", "USB serial");
        ui_message("\xE2\x9C\x93 Set up", "Connecting...");
    } else {
        ui_message("Lebenszeichen", "Connecting...");
    }
    board_light(LIGHT_OFF);

    lz_esp_hooks hooks;
    ui_hooks(&hooks);
    err = lz_esp_start(&hooks);
    if (err == ESP_ERR_INVALID_STATE)
        halt("State damaged", "Hold button 10 s at power-on");
    if (err != ESP_OK) halt("Start failed", esp_err_to_name(err));
    board_buttons_start(ui_wake);
}
