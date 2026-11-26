/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The Lebenszeichen task: owns the lz_ctx, feeds it what the network and
 * the buttons report and carries out its outputs in order (firmware/README.md,
 * "Rules for the adapter").
 */
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "bootloader_random.h"
#include "esp_log.h"
#include "esp_random.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "nvs.h"

#include "lz_esp_internal.h"

static const char *TAG = "lz";

#define NVS_NS "lz"
#define NVS_KEY "state"
#define QUEUE_LEN 32

static lz_ctx s_ctx;
static uint8_t *s_blob;
static QueueHandle_t s_q;
static lz_esp_hooks s_hooks;
static uint8_t s_erased;

/* ---- platform ---- */

static uint64_t pf_now(void *user)
{
    (void)user;
    time_t t = time(NULL);
    return t >= (time_t)LZ_MIN_TIME ? (uint64_t)t : 0;
}

static int pf_random(void *user, uint8_t *buf, size_t len)
{
    (void)user;
    /* A true random source while WiFi runs, or with bootloader_random
     * enabled (setup); the task only runs with WiFi started. */
    esp_fill_random(buf, len);
    return 0;
}

static const lz_platform s_pf = {NULL, pf_now, pf_random};

/* ---- storage ---- */

static esp_err_t blob_alloc(void)
{
    if (s_blob) return ESP_OK;
    s_blob = malloc(lz_save_max());
    return s_blob ? ESP_OK : ESP_ERR_NO_MEM;
}

static esp_err_t persist(void)
{
    size_t len = 0;
    if (blob_alloc() != ESP_OK) return ESP_ERR_NO_MEM;
    if (lz_save_buf(&s_ctx, s_blob, lz_save_max(), &len) != LZ_OK) return ESP_FAIL;
    nvs_handle_t h;
    esp_err_t err = nvs_open(NVS_NS, NVS_READWRITE, &h);
    if (err != ESP_OK) return err;
    err = nvs_set_blob(h, NVS_KEY, s_blob, len);
    if (err == ESP_OK) err = nvs_commit(h);
    nvs_close(h);
    lz_wipe(s_blob, len);
    return err;
}

int lz_esp_has_state(void)
{
    nvs_handle_t h;
    size_t len = 0;
    if (nvs_open(NVS_NS, NVS_READONLY, &h) != ESP_OK) return 0;
    esp_err_t err = nvs_get_blob(h, NVS_KEY, NULL, &len);
    nvs_close(h);
    return err == ESP_OK && len > 0;
}

esp_err_t lz_esp_erase(void)
{
    nvs_handle_t h;
    esp_err_t err = nvs_open(NVS_NS, NVS_READWRITE, &h);
    if (err != ESP_OK) return err;
    err = nvs_erase_all(h);
    if (err == ESP_OK) err = nvs_commit(h);
    nvs_close(h);
    return err;
}

int lz_esp_install(const uint8_t *setup, size_t len)
{
    if (s_q) return LZ_E_STATE; /* the task runs */
    lz_init(&s_ctx, &s_pf);
    bootloader_random_enable(); /* no radio yet: noise from the SAR ADC */
    int r = lz_install(&s_ctx, setup, len);
    bootloader_random_disable();
    if (r != LZ_OK) return r;
    if (persist() != ESP_OK) {
        lz_init(&s_ctx, &s_pf);
        return LZ_E_STATE;
    }
    return LZ_OK;
}

static esp_err_t load(void)
{
    nvs_handle_t h;
    size_t len = lz_save_max();
    if (blob_alloc() != ESP_OK) return ESP_ERR_NO_MEM;
    esp_err_t err = nvs_open(NVS_NS, NVS_READONLY, &h);
    if (err != ESP_OK) return ESP_ERR_NOT_FOUND;
    err = nvs_get_blob(h, NVS_KEY, s_blob, &len);
    nvs_close(h);
    if (err == ESP_ERR_NVS_NOT_FOUND) return ESP_ERR_NOT_FOUND;
    if (err != ESP_OK) return ESP_ERR_INVALID_STATE;
    lz_init(&s_ctx, &s_pf);
    int r = lz_load(&s_ctx, s_blob, len);
    lz_wipe(s_blob, len);
    return r == LZ_OK ? ESP_OK : ESP_ERR_INVALID_STATE;
}

/* ---- the queue ---- */

void lz_esp_post(const lz_esp_msg *m, uint32_t wait_ms)
{
    if (!s_q || xQueueSend(s_q, m, pdMS_TO_TICKS(wait_ms)) != pdTRUE) {
        if (s_q) ESP_LOGW(TAG, "queue full, dropped message %u", m->type);
        free(m->data);
    }
}

static void post_simple(uint8_t type, uint8_t button)
{
    lz_esp_msg m = {0};
    m.type = type;
    m.button = button;
    lz_esp_post(&m, 0);
}

void lz_esp_press(void) { post_simple(M_PRESS, 0); }
void lz_esp_long_press(void) { post_simple(M_LONG_PRESS, 0); }
void lz_esp_ask(uint8_t button) { post_simple(M_ASK, button); }

/* ---- outputs ---- */

static void drain(void)
{
    lz_output o;
    while (lz_poll(&s_ctx, &o)) {
        switch (o.type) {
        case LZ_OUT_PERSIST: {
            esp_err_t err = ESP_FAIL;
            for (int i = 0; i < 3 && err != ESP_OK; i++) err = persist();
            if (err != ESP_OK) {
                /* What follows relies on the stored state: rather start
                 * over from the last one than go on without it. */
                ESP_LOGE(TAG, "storing the state failed (%s), restarting", esp_err_to_name(err));
                esp_restart();
            }
            break;
        }
        case LZ_OUT_ERASE:
            for (uint8_t r = 0; r < LZ_MAX_RELAYS; r++) lz_net_ws_disconnect(r);
            if (lz_esp_erase() != ESP_OK) ESP_LOGE(TAG, "erasing the state failed");
            s_erased = 1;
            break;
        case LZ_OUT_EVENT:
            if (s_hooks.event) s_hooks.event(s_hooks.user, &s_ctx, &o.event);
            break;
        case LZ_OUT_VIEW:
            if (s_hooks.view) s_hooks.view(s_hooks.user, &s_ctx);
            break;
        case LZ_OUT_LED:
            if (s_hooks.led) s_hooks.led(s_hooks.user, o.led);
            break;
        case LZ_OUT_CONNECT:
            lz_net_ws_connect(o.relay, o.text);
            break;
        case LZ_OUT_DISCONNECT:
            lz_net_ws_disconnect(o.relay);
            break;
        case LZ_OUT_SEND:
            if (lz_net_ws_send(o.relay, o.text, o.len) != 0) {
                lz_esp_msg m = {0};
                m.type = M_WS_DOWN;
                m.relay = o.relay;
                m.gen = lz_net_ws_gen(o.relay);
                lz_esp_post(&m, 100);
            }
            break;
        case LZ_OUT_HTTP:
            lz_net_http(o.handle, o.method, o.url, o.authorization, o.content_type, o.body,
                        o.body_len);
            break;
        default:
            break;
        }
    }
}

static void handle(const lz_esp_msg *m)
{
    int r = LZ_OK;
    switch (m->type) {
    case M_ONLINE: r = lz_online(&s_ctx, 1); break;
    case M_OFFLINE: r = lz_online(&s_ctx, 0); break;
    case M_WS_UP:
        if (m->gen == lz_net_ws_gen(m->relay)) r = lz_connected(&s_ctx, m->relay, 1);
        break;
    case M_WS_DOWN:
        if (m->gen == lz_net_ws_gen(m->relay)) r = lz_connected(&s_ctx, m->relay, 0);
        break;
    case M_WS_FRAME:
        if (m->gen == lz_net_ws_gen(m->relay))
            r = lz_received(&s_ctx, m->relay, (const char *)m->data, m->len);
        break;
    case M_HTTP_OK:
        r = lz_http_response(&s_ctx, m->handle, m->status, m->date, m->data, m->len);
        break;
    case M_HTTP_FAIL: r = lz_http_failed(&s_ctx, m->handle); break;
    case M_PRESS: r = lz_press(&s_ctx); break;
    case M_LONG_PRESS: r = lz_long_press(&s_ctx); break;
    case M_ASK: r = lz_ask(&s_ctx, m->button); break;
    case M_STATUS:
        if (s_hooks.status) {
            lz_esp_status st;
            lz_net_status(&st);
            s_hooks.status(s_hooks.user, &st);
        }
        break;
    default: break;
    }
    if (r != LZ_OK) ESP_LOGD(TAG, "input %u: %d", m->type, r);
}

static void task(void *arg)
{
    (void)arg;
    lz_tick(&s_ctx);
    drain();
    int64_t last = esp_timer_get_time();
    for (;;) {
        lz_esp_msg m;
        if (xQueueReceive(s_q, &m, pdMS_TO_TICKS(200)) == pdTRUE) {
            handle(&m);
            if (m.data) {
                lz_wipe(m.data, m.len);
                free(m.data);
            }
            drain();
        }
        int64_t t = esp_timer_get_time();
        if (t - last >= 1000000) {
            last = t;
            lz_tick(&s_ctx);
            drain();
            if (s_hooks.tick) s_hooks.tick(s_hooks.user, &s_ctx);
        }
        if (s_erased) {
            ESP_LOGI(TAG, "removed: state erased");
            if (s_hooks.erased) s_hooks.erased(s_hooks.user);
            esp_restart();
        }
    }
}

esp_err_t lz_esp_start(const lz_esp_hooks *hooks)
{
    if (s_q) return ESP_ERR_INVALID_STATE;
    esp_err_t err = load();
    if (err != ESP_OK) return err;
    s_hooks = *hooks;
    s_q = xQueueCreate(QUEUE_LEN, sizeof(lz_esp_msg));
    if (!s_q) return ESP_ERR_NO_MEM;

    lz_net_set_relays(s_ctx.nrelays);
    const uint8_t *name, *pw;
    size_t name_len, pw_len;
    lz_wifi(&s_ctx, &name, &name_len, &pw, &pw_len);
    lz_net_start(name, name_len, pw, pw_len);

    if (xTaskCreate(task, "lz", CONFIG_LZ_TASK_STACK, NULL, 5, NULL) != pdPASS)
        return ESP_ERR_NO_MEM;
    return ESP_OK;
}
