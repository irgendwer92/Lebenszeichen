/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <math.h>

#include "board.h"
#include "driver/gpio.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "led_strip.h"
#include "lz_esp.h"
#include "sdkconfig.h"

static const char *TAG = "board";

#define LONG_PRESS_MS 1500
#define POLL_MS 10

static led_strip_handle_t s_led;
static volatile int s_light, s_flash_until_ms;

static void led_set(uint8_t r, uint8_t g, uint8_t b)
{
    if (!s_led) return;
    led_strip_set_pixel(s_led, 0, r, g, b);
    led_strip_refresh(s_led);
}

static void led_task(void *arg)
{
    (void)arg;
    uint8_t last[3] = {1, 1, 1};
    for (uint32_t t = 0;; t += 20) {
        uint8_t c[3] = {0, 0, 0};
        int light = s_light;
        uint32_t now = (uint32_t)(xTaskGetTickCount() * portTICK_PERIOD_MS);
        if (s_flash_until_ms && (int32_t)(now - (uint32_t)s_flash_until_ms) < 0) light = LIGHT_FLASH;
        switch (light) {
        case LIGHT_REQUEST: {
            /* 3-second breathing, never quite off. */
            float k = 0.5f - 0.5f * cosf((float)(t % 3000) / 3000.0f * 2.0f * (float)M_PI);
            c[2] = (uint8_t)(4 + 36 * k);
            break;
        }
        case LIGHT_ANSWER: c[1] = 12; break;
        case LIGHT_REPORTED: c[1] = 10; c[2] = 8; break;
        case LIGHT_STATE: c[0] = 10; c[1] = 6; break;
        case LIGHT_WARNING: if (t % 1000 < 150) c[0] = 30; break;
        case LIGHT_SETUP: c[0] = 14; c[2] = 18; break;
        case LIGHT_FLASH: c[1] = 40; break;
        default: break;
        }
        if (c[0] != last[0] || c[1] != last[1] || c[2] != last[2]) {
            led_set(c[0], c[1], c[2]);
            last[0] = c[0];
            last[1] = c[1];
            last[2] = c[2];
        }
        vTaskDelay(pdMS_TO_TICKS(20));
    }
}

void board_light(int light)
{
    if (light == LIGHT_FLASH) {
        uint32_t now = (uint32_t)(xTaskGetTickCount() * portTICK_PERIOD_MS);
        s_flash_until_ms = (int)(now + 2000) | 1;
    } else {
        s_light = light;
    }
}

esp_err_t board_init(void)
{
    gpio_config_t io = {
        .pin_bit_mask = (1ULL << CONFIG_SAT_PIN_ANSWER) | (1ULL << CONFIG_SAT_PIN_ASK),
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
    };
    esp_err_t err = gpio_config(&io);
    if (err != ESP_OK) return err;

    led_strip_config_t sc = {
        .strip_gpio_num = CONFIG_SAT_PIN_LED,
        .max_leds = 1,
        .led_model = LED_MODEL_WS2812,
        .color_component_format = LED_STRIP_COLOR_COMPONENT_FMT_GRB,
    };
    led_strip_rmt_config_t rc = {
        .clk_src = RMT_CLK_SRC_DEFAULT,
        .resolution_hz = 10 * 1000 * 1000,
    };
    if (led_strip_new_rmt_device(&sc, &rc, &s_led) != ESP_OK) {
        ESP_LOGW(TAG, "no RGB LED");
        s_led = NULL;
    }
    led_set(0, 0, 0);
    xTaskCreate(led_task, "led", 2048, NULL, 3, NULL);
    vTaskDelay(pdMS_TO_TICKS(30)); /* let the pull-ups settle */
    return ESP_OK;
}

int board_answer_held(void)
{
    return gpio_get_level(CONFIG_SAT_PIN_ANSWER) == 0;
}

static void (*s_on_any)(void);

static void buttons_task(void *arg)
{
    (void)arg;
    /* Debounced: a level counts after 3 equal samples (30 ms). */
    int ans = 1, ans_n = 0, ask = 1, ask_n = 0;
    uint32_t held_ms = 0;
    int long_sent = 0;
    /* A button held through boot (the reset countdown) counts only after
     * it was released once. */
    int armed = !board_answer_held();
    for (;;) {
        int a = gpio_get_level(CONFIG_SAT_PIN_ANSWER), k = gpio_get_level(CONFIG_SAT_PIN_ASK);
        ans_n = a == ans ? 0 : ans_n + 1;
        if (ans_n >= 3) {
            ans = a;
            ans_n = 0;
            if (ans == 0 && armed) {
                held_ms = 0;
                long_sent = 0;
                if (s_on_any) s_on_any();
            } else if (ans == 1) {
                if (armed && !long_sent) lz_esp_press();
                armed = 1;
            }
        }
        if (ans == 0 && armed && !long_sent) {
            held_ms += POLL_MS;
            if (held_ms >= LONG_PRESS_MS) {
                long_sent = 1;
                lz_esp_long_press();
            }
        }
        ask_n = k == ask ? 0 : ask_n + 1;
        if (ask_n >= 3) {
            ask = k;
            ask_n = 0;
            if (ask == 0) {
                if (s_on_any) s_on_any();
                lz_esp_ask(1);
            }
        }
        vTaskDelay(pdMS_TO_TICKS(POLL_MS));
    }
}

void board_buttons_start(void (*on_any)(void))
{
    s_on_any = on_any;
    xTaskCreate(buttons_task, "buttons", 3072, NULL, 4, NULL);
}
