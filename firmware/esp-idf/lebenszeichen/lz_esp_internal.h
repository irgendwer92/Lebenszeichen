/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* Between the task (lz_esp.c) and the network side (lz_esp_net.c). */
#ifndef LZ_ESP_INTERNAL_H
#define LZ_ESP_INTERNAL_H

#include <stddef.h>
#include <stdint.h>

#include "lz_esp.h"

enum {
    M_ONLINE = 1,
    M_OFFLINE,
    M_WS_UP,
    M_WS_DOWN,
    M_WS_FRAME,
    M_HTTP_OK,
    M_HTTP_FAIL,
    M_PRESS,
    M_LONG_PRESS,
    M_ASK,
    M_STATUS
};

typedef struct {
    uint8_t type;
    uint8_t relay;
    uint8_t button;
    uint32_t gen;    /* WebSocket messages: the connection they belong to */
    uint32_t handle; /* HTTP */
    int status;
    uint64_t date;
    uint8_t *data; /* malloc'ed, freed by the task */
    size_t len;
} lz_esp_msg;

/* Queues a message for the task; takes ownership of data. wait_ms how long
 * to wait for room; dropped (and data freed) when the queue stays full. */
void lz_esp_post(const lz_esp_msg *m, uint32_t wait_ms);

/* The network side; all called from the task except where noted. */
void lz_net_start(const uint8_t *ssid, size_t ssid_len, const uint8_t *pw, size_t pw_len);
void lz_net_ws_connect(uint8_t relay, const char *url);
void lz_net_ws_disconnect(uint8_t relay);
/* 0 on success. */
int lz_net_ws_send(uint8_t relay, const char *text, size_t len);
uint32_t lz_net_ws_gen(uint8_t relay);
void lz_net_http(uint32_t handle, const char *method, const char *url, const char *auth,
                 const char *content_type, const uint8_t *body, size_t body_len);
void lz_net_status(lz_esp_status *st);
void lz_net_set_relays(uint8_t n);

/* RFC 7231 IMF-fixdate ("Sun, 06 Nov 1994 08:49:37 GMT") as Unix seconds,
 * 0 if it does not parse. */
uint64_t lz_esp_parse_http_date(const char *s);

#endif
