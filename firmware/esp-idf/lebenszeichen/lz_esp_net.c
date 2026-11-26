/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * WiFi, NTP, the WebSocket connections to the relays and HTTPS, all with
 * the certificate bundle. Everything that arrives goes to the task's queue
 * as a message; the WebSocket and HTTP callbacks run in their own tasks.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "esp_crt_bundle.h"
#include "esp_event.h"
#include "esp_http_client.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_netif_sntp.h"
#include "esp_timer.h"
#include "esp_websocket_client.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

#include "lz_esp_internal.h"

static const char *TAG = "lz-net";

#define MAX_FRAME 4096
#define MAX_BODY 4096

/* ---- status ---- */

static volatile uint8_t s_wifi_up, s_time_set, s_nrelays;

static void post_status(void)
{
    lz_esp_msg m = {0};
    m.type = M_STATUS;
    lz_esp_post(&m, 0);
}

/* ---- WebSocket ---- */

typedef struct {
    esp_websocket_client_handle_t client;
    uint8_t relay;
    uint32_t gen;
    volatile uint8_t up;
    volatile uint8_t down_sent;
    uint8_t *frame;
    size_t frame_len, frame_cap;
} ws_conn;

static ws_conn *s_ws[LZ_MAX_RELAYS];
static uint32_t s_gen[LZ_MAX_RELAYS];

uint32_t lz_net_ws_gen(uint8_t relay)
{
    return relay < LZ_MAX_RELAYS ? s_gen[relay] : 0;
}

void lz_net_set_relays(uint8_t n)
{
    s_nrelays = n;
}

static void ws_post(ws_conn *c, uint8_t type, uint8_t *data, size_t len)
{
    lz_esp_msg m = {0};
    m.type = type;
    m.relay = c->relay;
    m.gen = c->gen;
    m.data = data;
    m.len = len;
    lz_esp_post(&m, type == M_WS_FRAME ? 1000 : 100);
}

static void ws_down(ws_conn *c)
{
    c->up = 0;
    if (c->down_sent) return;
    c->down_sent = 1;
    ws_post(c, M_WS_DOWN, NULL, 0);
    post_status();
}

static void ws_event(void *arg, esp_event_base_t base, int32_t id, void *event_data)
{
    (void)base;
    ws_conn *c = arg;
    const esp_websocket_event_data_t *d = event_data;
    switch (id) {
    case WEBSOCKET_EVENT_CONNECTED:
        c->up = 1;
        ws_post(c, M_WS_UP, NULL, 0);
        post_status();
        break;
    case WEBSOCKET_EVENT_DISCONNECTED:
    case WEBSOCKET_EVENT_CLOSED:
    case WEBSOCKET_EVENT_ERROR:
        ws_down(c);
        break;
    case WEBSOCKET_EVENT_DATA:
        /* Text frames only; a longer one arrives in pieces (payload_offset). */
        if (d->op_code != 0x1 && !(d->op_code == 0x0 && c->frame)) break;
        if (d->payload_offset == 0) {
            free(c->frame);
            c->frame = NULL;
            c->frame_len = 0;
            if (d->payload_len <= 0 || d->payload_len > MAX_FRAME) break;
            c->frame_cap = (size_t)d->payload_len;
            c->frame = malloc(c->frame_cap);
            if (!c->frame) break;
        }
        if (!c->frame || d->data_len < 0 || c->frame_len + (size_t)d->data_len > c->frame_cap)
            break;
        memcpy(c->frame + c->frame_len, d->data_ptr, (size_t)d->data_len);
        c->frame_len += (size_t)d->data_len;
        if (c->frame_len == c->frame_cap) {
            ws_post(c, M_WS_FRAME, c->frame, c->frame_len);
            c->frame = NULL;
            c->frame_len = 0;
        }
        break;
    default:
        break;
    }
}

void lz_net_ws_disconnect(uint8_t relay)
{
    if (relay >= LZ_MAX_RELAYS) return;
    ws_conn *c = s_ws[relay];
    s_gen[relay]++; /* whatever this connection still queued is ignored */
    if (!c) return;
    s_ws[relay] = NULL;
    esp_websocket_client_destroy(c->client);
    free(c->frame);
    free(c);
    post_status();
}

void lz_net_ws_connect(uint8_t relay, const char *url)
{
    if (relay >= LZ_MAX_RELAYS) return;
    lz_net_ws_disconnect(relay);
    ws_conn *c = calloc(1, sizeof(*c));
    if (!c) goto fail;
    c->relay = relay;
    c->gen = s_gen[relay];

    esp_websocket_client_config_t cfg = {
        .uri = url,
        .crt_bundle_attach = esp_crt_bundle_attach,
        .disable_auto_reconnect = true, /* the core reconnects with backoff */
        .ping_interval_sec = 30,        /* keeps the reverse proxy from closing it */
        .pingpong_timeout_sec = 90,
        .network_timeout_ms = 15000,
        .buffer_size = 2048,
        .task_stack = 8192,
    };
    c->client = esp_websocket_client_init(&cfg);
    if (!c->client) goto fail;
    esp_websocket_register_events(c->client, WEBSOCKET_EVENT_ANY, ws_event, c);
    s_ws[relay] = c;
    if (esp_websocket_client_start(c->client) != ESP_OK) {
        lz_net_ws_disconnect(relay);
        c = NULL;
        goto fail;
    }
    return;
fail:
    if (c) {
        if (c->client) esp_websocket_client_destroy(c->client);
        free(c);
    }
    {
        lz_esp_msg m = {0};
        m.type = M_WS_DOWN;
        m.relay = relay;
        m.gen = s_gen[relay];
        lz_esp_post(&m, 100);
    }
}

int lz_net_ws_send(uint8_t relay, const char *text, size_t len)
{
    if (relay >= LZ_MAX_RELAYS || !s_ws[relay] || !s_ws[relay]->up) return -1;
    int n = esp_websocket_client_send_text(s_ws[relay]->client, text, (int)len,
                                           pdMS_TO_TICKS(5000));
    return n == (int)len ? 0 : -1;
}

/* ---- HTTP: one request at a time, in its own task ---- */

typedef struct {
    uint32_t handle;
    esp_http_client_method_t method;
    char *url, *auth, *ctype;
    uint8_t *body;
    size_t body_len;
} http_job;

typedef struct {
    uint8_t *buf;
    size_t len;
    uint8_t over;
    uint64_t date;
} http_resp;

static QueueHandle_t s_http_q;

static char *dup_or_null(const char *s)
{
    return s ? strdup(s) : NULL;
}

static void job_free(http_job *j)
{
    free(j->url);
    if (j->auth) {
        lz_wipe(j->auth, strlen(j->auth));
        free(j->auth);
    }
    free(j->ctype);
    free(j->body);
}

static esp_err_t http_event(esp_http_client_event_t *e)
{
    http_resp *r = e->user_data;
    switch (e->event_id) {
    case HTTP_EVENT_ON_HEADER:
        if (e->header_key && e->header_value && strcasecmp(e->header_key, "Date") == 0)
            r->date = lz_esp_parse_http_date(e->header_value);
        break;
    case HTTP_EVENT_ON_DATA:
        if (e->data_len <= 0) break;
        if (r->len + (size_t)e->data_len > MAX_BODY) {
            r->over = 1;
            break;
        }
        memcpy(r->buf + r->len, e->data, (size_t)e->data_len);
        r->len += (size_t)e->data_len;
        break;
    default:
        break;
    }
    return ESP_OK;
}

static void http_task(void *arg)
{
    (void)arg;
    static uint8_t buf[MAX_BODY];
    for (;;) {
        http_job j;
        if (xQueueReceive(s_http_q, &j, portMAX_DELAY) != pdTRUE) continue;
        http_resp r = {buf, 0, 0, 0};
        esp_http_client_config_t cfg = {
            .url = j.url,
            .method = j.method,
            .crt_bundle_attach = esp_crt_bundle_attach,
            .event_handler = http_event,
            .user_data = &r,
            .timeout_ms = 20000,
            .disable_auto_redirect = true,
            .buffer_size = 2048,
            .buffer_size_tx = 1024,
        };
        esp_http_client_handle_t c = esp_http_client_init(&cfg);
        esp_err_t err = c ? ESP_OK : ESP_ERR_NO_MEM;
        if (c) {
            if (j.auth) esp_http_client_set_header(c, "Authorization", j.auth);
            if (j.ctype) esp_http_client_set_header(c, "Content-Type", j.ctype);
            if (j.body) esp_http_client_set_post_field(c, (const char *)j.body, (int)j.body_len);
            err = esp_http_client_perform(c);
        }
        lz_esp_msg m = {0};
        m.handle = j.handle;
        if (err == ESP_OK && !r.over) {
            m.type = M_HTTP_OK;
            m.status = esp_http_client_get_status_code(c);
            m.date = r.date;
            if (r.len) {
                m.data = malloc(r.len);
                if (m.data) {
                    memcpy(m.data, r.buf, r.len);
                    m.len = r.len;
                } else {
                    m.type = M_HTTP_FAIL;
                }
            }
        } else {
            ESP_LOGW(TAG, "HTTP %s failed: %s", j.url, esp_err_to_name(err));
            m.type = M_HTTP_FAIL;
        }
        if (c) esp_http_client_cleanup(c);
        job_free(&j);
        lz_esp_post(&m, 1000);
    }
}

void lz_net_http(uint32_t handle, const char *method, const char *url, const char *auth,
                 const char *content_type, const uint8_t *body, size_t body_len)
{
    http_job j = {0};
    j.handle = handle;
    if (strcmp(method, "GET") == 0) j.method = HTTP_METHOD_GET;
    else if (strcmp(method, "PUT") == 0) j.method = HTTP_METHOD_PUT;
    else if (strcmp(method, "POST") == 0) j.method = HTTP_METHOD_POST;
    else if (strcmp(method, "DELETE") == 0) j.method = HTTP_METHOD_DELETE;
    else goto fail;
    j.url = dup_or_null(url);
    j.auth = dup_or_null(auth);
    j.ctype = dup_or_null(content_type);
    if (body && body_len) {
        j.body = malloc(body_len);
        if (j.body) memcpy(j.body, body, body_len);
        j.body_len = body_len;
    }
    if (!j.url || (auth && !j.auth) || (content_type && !j.ctype) || (body_len && !j.body))
        goto fail;
    if (xQueueSend(s_http_q, &j, 0) == pdTRUE) return;
fail:
    job_free(&j);
    lz_esp_msg m = {0};
    m.type = M_HTTP_FAIL;
    m.handle = handle;
    lz_esp_post(&m, 100);
}

/* ---- WiFi and NTP ---- */

static esp_timer_handle_t s_retry;
static uint32_t s_retry_s = 1;

static void retry_cb(void *arg)
{
    (void)arg;
    esp_wifi_connect();
}

static void wifi_event(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    (void)arg;
    (void)data;
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_START) {
        esp_wifi_connect();
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        if (s_wifi_up) {
            s_wifi_up = 0;
            lz_esp_msg m = {0};
            m.type = M_OFFLINE;
            lz_esp_post(&m, 100);
            post_status();
        }
        esp_timer_start_once(s_retry, (uint64_t)s_retry_s * 1000000u);
        if (s_retry_s < 60) s_retry_s *= 2;
    } else if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        s_retry_s = 1;
        s_wifi_up = 1;
        lz_esp_msg m = {0};
        m.type = M_ONLINE;
        lz_esp_post(&m, 100);
        post_status();
    }
}

static void time_synced(struct timeval *tv)
{
    (void)tv;
    s_time_set = 1;
    post_status();
}

void lz_net_status(lz_esp_status *st)
{
    memset(st, 0, sizeof(*st));
    st->wifi = s_wifi_up;
    st->time = s_time_set;
    st->relays = s_nrelays;
    for (uint8_t r = 0; r < LZ_MAX_RELAYS; r++)
        if (s_ws[r] && s_ws[r]->up) st->connected++;
}

void lz_net_start(const uint8_t *ssid, size_t ssid_len, const uint8_t *pw, size_t pw_len)
{
    ESP_ERROR_CHECK(esp_netif_init());
    esp_err_t err = esp_event_loop_create_default();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) ESP_ERROR_CHECK(err);
    esp_netif_t *netif = esp_netif_create_default_wifi_sta();
    esp_netif_set_hostname(netif, "lebenszeichen");

    s_http_q = xQueueCreate(2, sizeof(http_job));
    xTaskCreate(http_task, "lz-http", 8192, NULL, 5, NULL);

    const esp_timer_create_args_t targs = {.callback = retry_cb, .name = "lz-wifi"};
    ESP_ERROR_CHECK(esp_timer_create(&targs, &s_retry));

    esp_sntp_config_t sntp = ESP_NETIF_SNTP_DEFAULT_CONFIG(CONFIG_LZ_NTP_SERVER);
    sntp.sync_cb = time_synced;
    esp_netif_sntp_init(&sntp);

    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init));
    /* The WiFi password lives in the state blob only. */
    ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
    ESP_ERROR_CHECK(esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event, NULL));

    wifi_config_t wc;
    memset(&wc, 0, sizeof(wc));
    if (ssid_len > sizeof(wc.sta.ssid)) ssid_len = sizeof(wc.sta.ssid);
    if (pw_len > sizeof(wc.sta.password)) pw_len = sizeof(wc.sta.password);
    memcpy(wc.sta.ssid, ssid, ssid_len);
    memcpy(wc.sta.password, pw, pw_len);
    wc.sta.threshold.authmode = pw_len ? WIFI_AUTH_WPA2_PSK : WIFI_AUTH_OPEN;
    wc.sta.pmf_cfg.capable = true;
    wc.sta.sae_pwe_h2e = WPA3_SAE_PWE_BOTH;
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &wc));
    lz_wipe(&wc, sizeof(wc));
    ESP_ERROR_CHECK(esp_wifi_start());
    ESP_LOGI(TAG, "WiFi started");
}

/* ---- HTTP date ---- */

static int64_t days_from_civil(int64_t y, unsigned m, unsigned d)
{
    y -= m <= 2;
    const int64_t era = (y >= 0 ? y : y - 399) / 400;
    const unsigned yoe = (unsigned)(y - era * 400);
    const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + (int64_t)doe - 719468;
}

uint64_t lz_esp_parse_http_date(const char *s)
{
    static const char months[] = "JanFebMarAprMayJunJulAugSepOctNovDec";
    unsigned d, y, hh, mm, ss;
    char mon[4];
    const char *p = strchr(s, ',');
    if (!p) return 0;
    if (sscanf(p + 1, " %2u %3s %4u %2u:%2u:%2u", &d, mon, &y, &hh, &mm, &ss) != 6) return 0;
    const char *f = strstr(months, mon);
    if (!f || strlen(mon) != 3 || (f - months) % 3) return 0;
    unsigned m = (unsigned)(f - months) / 3 + 1;
    if (d < 1 || d > 31 || y < 1970 || hh > 23 || mm > 59 || ss > 60) return 0;
    int64_t t = days_from_civil(y, m, d) * 86400 + hh * 3600 + mm * 60 + ss;
    return t > 0 ? (uint64_t)t : 0;
}
