/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * lz_core: the Lebenszeichen satellite (docs/m10-satellite.md, part 2) as a
 * pure state machine.
 *
 * The caller owns one lz_ctx (static or on the heap, about 50 KB with the
 * default limits) and drives it:
 *
 *   lz_init(&ctx, &platform);
 *   lz_load(&ctx, blob, len)      or  lz_install(&ctx, setup, len);
 *   loop:
 *     feed inputs as they happen:  lz_feed(&ctx, &input)  (or the helpers)
 *     then drain the outputs:      while (lz_poll(&ctx, &out)) handle(&out);
 *
 * Inputs are events from the outside world (network up, relay connected,
 * a WebSocket text frame, an HTTP response, a button, a tick at least once
 * a second). Outputs are actions for the adapter (persist the state, open
 * a WebSocket, send a frame, make an HTTP request, update display and
 * light) and events for the application (a request opened or closed, an
 * answer to an own ask arrived, ...). Outputs must be handled in the order
 * they come: LZ_OUT_PERSIST must be stored durably before the next output
 * is handled, because frames and requests that follow rely on it (an
 * acknowledgement after the message's effect was stored, a new token stored
 * before it is sent).
 *
 * Pointers in an lz_output stay valid until the next lz_poll() or
 * lz_feed() on the same context.
 *
 * Time and randomness come through lz_platform. The core does no I/O, no
 * allocation and keeps no global mutable state.
 */
#ifndef LZ_LZ_H
#define LZ_LZ_H

#include <stddef.h>
#include <stdint.h>

#include "lz/config.h"
#include "lz/crypto.h"
#include "lz/relay.h"
#include "lz/wire.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Time before which the clock counts as unset (2024-01-01). */
#define LZ_MIN_TIME 1704067200u
/* Largest difference to the relay's Date header before the satellite
 * stops sending (M9, decision 3). */
#define LZ_MAX_CLOCK_SKEW 300u
/* How long the answer to an own request stays shown (decision 13). */
#define LZ_ANSWER_SHOWN_SECS (24u * 3600u)

typedef struct {
    void *user;
    /* Unix seconds (UTC), or 0 while the clock is not set (no NTP yet). */
    uint64_t (*now)(void *user);
    /* Fills buf from a cryptographic random source; 0 on success. A failure
     * makes the operation that needed randomness fail; there is no
     * fallback. */
    int (*random)(void *user, uint8_t *buf, size_t len);
} lz_platform;

/* ---- the state (treat as opaque; fields may change between versions) ---- */

typedef struct {
    uint8_t id[16];
    uint64_t at;
    uint64_t answered_at;
    uint8_t answered;
    uint8_t reported;
} lz_entry;

typedef struct {
    lz_entry e[LZ_MAX_REQUESTS];
    uint8_t n;
    uint8_t has_last_answer;
    uint64_t last_answer;
} lz_ledger;

typedef struct {
    uint8_t id[16];
    uint64_t at;
} lz_seen_entry;

typedef struct {
    uint8_t has;
    lz_reason reason;
    uint64_t until;
    uint64_t since;
    uint8_t shared;
} lz_state_slot;

/* Per mailbox, not persisted. */
typedef struct {
    uint8_t sub;       /* 0 none, 1 sent, 2 subscribed */
    uint8_t sub_own;   /* subscribed with the own token */
    uint8_t need_sub;  /* send subscribe when connected */
    uint8_t need_ack;
    uint8_t lost;      /* unauthorized with the own token: stop */
    uint8_t token_job; /* 0 idle, 1 in flight, 2 failed for good */
    uint8_t token_try; /* 0 with the enrolment token, 1 with the new */
    uint64_t ack_seq;
    uint64_t token_retry_at;
} lz_mbox_rt;

typedef struct {
    uint8_t own_mailbox[32];
    uint8_t own_relay; /* index into lz_ctx.relays */
    uint8_t enrolment[32];
    uint8_t has_own_token;
    uint8_t own_token[32];
    uint8_t has_new_token;
    uint8_t new_token[32];
    lz_cname name;
    uint8_t secret_key[32];
    uint8_t peer_key[32];
    uint8_t peer_mailbox[32];
    uint8_t peer_send_token[32];
    lz_url peer_relay;
    lz_rules own_rules;
    lz_stamp own_rules_stamp;
    lz_rules peer_rules;
    lz_stamp peer_rules_stamp;
    lz_flags flags;
    lz_stamp flags_stamp;
    uint8_t ended;
    lz_ledger incoming;
    lz_ledger outgoing;
    lz_seen_entry seen[LZ_MAX_SEEN];
    uint8_t seen_n;
    uint64_t seen_floor;
    /* The answer to an own request on display. */
    uint8_t answer_shown;
    uint8_t answer_kind;
    lz_reason answer_reason;
    uint64_t answer_at;
    uint64_t answer_until;
    /* Not persisted: derived keys and the last open request reported. */
    uint8_t keys_ready;
    uint8_t own_pk[32];
    uint8_t shared[32];
    uint8_t was_open;
    uint8_t was_open_id[16];
} lz_contact;

typedef struct {
    uint8_t relay;
    uint8_t mailbox[32];
    uint8_t enrolment[32];
    uint8_t has_own_token;
    uint8_t own_token[32];
    uint8_t has_new_token;
    uint8_t new_token[32];
    uint8_t send_token[32];
    uint8_t key[32];
    lz_seen_entry seen[LZ_MAX_CHANNEL_SEEN];
    uint8_t seen_n;
    uint64_t seen_floor;
} lz_channel;

typedef struct {
    uint8_t used;
    uint8_t contact[32];
    uint8_t transfer[16];
    uint8_t count;
    uint8_t discarded;
    uint8_t have; /* bit i: part i arrived */
    uint8_t lens[LZ_RECORD_MAX_PARTS];
    uint8_t data[LZ_RECORD_MAX_PARTS][LZ_MAX_PART_LEN];
    uint64_t first_seen;
} lz_transfer;

typedef struct {
    uint8_t plain[LZ_PLAINTEXT_LEN];
    uint64_t arrived;
} lz_parked;

typedef struct {
    uint8_t to_channel;
    uint8_t contact[32]; /* own mailbox of the contact, if not to_channel */
    uint8_t kind;
    uint64_t created_at;
    uint8_t env[LZ_ENVELOPE_LEN];
    /* Not persisted. */
    uint64_t next_try;
    uint32_t backoff;
} lz_out_entry;

typedef struct {
    lz_url url;
    /* Not persisted. */
    uint8_t conn; /* 0 down, 1 connecting, 2 connected */
    uint8_t health; /* 0 to do, 1 in flight, 2 done */
    uint8_t reader_tokens;
    uint32_t backoff;
    uint64_t next_connect;
    uint64_t connect_started;
} lz_relay;

/* Events for the application. */
enum {
    LZ_EV_REQUEST_OPENED = 1, /* contact: a request is shown */
    LZ_EV_REQUEST_CLOSED,     /* contact: answered here or elsewhere, or expired */
    LZ_EV_ANSWER,             /* contact, kind, reason: the answer to an own ask */
    LZ_EV_ASKED,              /* contact: an ask went out */
    LZ_EV_ASK_REFUSED,        /* contact (0xFF if none bound), at: possible again */
    LZ_EV_ANSWERED,           /* contact, kind: this satellite answered */
    LZ_EV_STATE,              /* the reported state that applies changed */
    LZ_EV_CONTACT_ADDED,      /* contact */
    LZ_EV_CONTACT_UPDATED,    /* contact */
    LZ_EV_CONTACT_ENDED,      /* contact */
    LZ_EV_CONTACT_REMOVED,    /* name only: its mailbox is gone */
    LZ_EV_CONTACT_REFUSED,    /* name only: over a limit or unusable */
    LZ_EV_CLOCK_WARNING,      /* at: the relay's time; nothing is sent */
    LZ_EV_CLOCK_OK,
    LZ_EV_ACCESS_LOST,        /* contact (0xFF: channel): token refused */
    LZ_EV_NO_READER_TOKENS,   /* relay index: the relay lacks reader tokens */
    LZ_EV_OUTBOX_FULL,
    LZ_EV_REMOVED             /* removed on the phone: everything is erased */
};

typedef struct {
    int type;
    uint8_t contact; /* index, 0xFF for none */
    uint8_t kind;    /* LZ_KIND_* */
    uint8_t relay;
    uint64_t at;
    lz_reason reason;
    lz_cname name; /* the contact's name at the time */
} lz_event;

typedef struct lz_ctx {
    lz_platform pf;
    uint8_t installed;
    uint8_t removed;
    uint8_t reader[16];
    uint8_t wifi_name_len;
    uint8_t wifi_name[LZ_MAX_WIFI_NAME_LEN];
    uint8_t wifi_password_len;
    uint8_t wifi_password[LZ_MAX_WIFI_PASSWORD_LEN];
    lz_relay relays[LZ_MAX_RELAYS];
    uint8_t nrelays;
    lz_channel channel;
    lz_settings settings;
    lz_stamp settings_stamp;
    lz_state_slot own_state;
    lz_state_slot handed;
    lz_stamp share_stamp;
    lz_contact contacts[LZ_MAX_CONTACTS];
    uint8_t ncontacts;
    lz_transfer transfers[LZ_MAX_TRANSFERS];
    lz_parked parked[LZ_MAX_PARKED];
    uint8_t nparked;
    lz_out_entry outbox[LZ_OUTBOX_SIZE];
    uint8_t noutbox;

    /* Not persisted. */
    lz_mbox_rt rt[1 + LZ_MAX_CONTACTS]; /* 0: channel, 1 + i: contact i */
    lz_event events[LZ_MAX_EVENTS];
    uint8_t ev_head, ev_count;
    uint8_t dirty, view_dirty, erase, online, clock_bad, clock_checked, led, led_sent;
    lz_state_slot state_last;
    uint8_t http_busy, http_kind, http_relay, http_try;
    uint8_t http_target[32]; /* the mailbox, or the start of the envelope */
    uint32_t http_handle;
    uint64_t http_deadline;
    uint64_t now;
    uint64_t health_due;
    char text[640];
    char url[320];
    char auth[64];
} lz_ctx;

/* ---- inputs ---- */

enum {
    LZ_IN_TICK = 0,      /* at least once a second */
    LZ_IN_ONLINE,        /* the network is up (WiFi associated, IP) */
    LZ_IN_OFFLINE,       /* the network is down */
    LZ_IN_CONNECTED,     /* relay: the WebSocket is open */
    LZ_IN_DISCONNECTED,  /* relay: closed, or connecting failed */
    LZ_IN_FRAME,         /* relay, data/len: a text frame */
    LZ_IN_HTTP_RESPONSE, /* handle, status, date, data/len: the body */
    LZ_IN_HTTP_FAILED,   /* handle: no response (DNS, TLS, timeout) */
    LZ_IN_PRESS,         /* the answer button */
    LZ_IN_LONG_PRESS,    /* the answer button held: the preset */
    LZ_IN_ASK            /* button: an ask button, from 1 */
};

typedef struct {
    int type;
    uint8_t relay;
    uint8_t button;
    uint32_t handle;
    int status;
    uint64_t date; /* the response's Date header as Unix seconds, 0 if none */
    const uint8_t *data;
    size_t len;
} lz_input;

/* ---- outputs ---- */

enum {
    LZ_OUT_NONE = 0,
    LZ_OUT_ERASE,      /* erase the stored state (removed) */
    LZ_OUT_PERSIST,    /* store lz_save() now, before the next output */
    LZ_OUT_EVENT,      /* event */
    LZ_OUT_VIEW,       /* the display changed: render lz_view() */
    LZ_OUT_LED,        /* led */
    LZ_OUT_CONNECT,    /* relay, text: open a WebSocket to this address */
    LZ_OUT_DISCONNECT, /* relay */
    LZ_OUT_SEND,       /* relay, text/len: send a text frame */
    LZ_OUT_HTTP        /* handle, method, url, authorization, content_type, body */
};

enum {
    LZ_LED_OFF = 0,
    LZ_LED_REQUEST,         /* a request is open */
    LZ_LED_ANSWER_MANUAL,   /* an own ask was answered */
    LZ_LED_ANSWER_REPORTED, /* an own ask got a reported answer */
    LZ_LED_WARNING          /* clock off, or access lost */
};

typedef struct {
    int type;
    uint8_t relay;
    const char *text;
    size_t len;
    uint32_t handle;
    const char *method;
    const char *url;
    const char *authorization; /* the full header value, "Bearer ..." */
    const char *content_type;  /* NULL without body */
    const uint8_t *body;
    size_t body_len;
    int led;
    lz_event event;
} lz_output;

/* ---- the API ---- */

/* Zeroes the context and sets the platform; then lz_load or lz_install. */
int lz_init(lz_ctx *ctx, const lz_platform *pf);

/* Installs a setup (wire-format.md, "Satellite setup"). Refuses more
 * contacts than LZ_MAX_CONTACTS, more relays than LZ_MAX_RELAYS, unusable
 * relay addresses and duplicate contacts; the context is then unchanged. */
int lz_install(lz_ctx *ctx, const uint8_t *setup, size_t len);

/* Feeds one input. Returns LZ_OK, or an error for an invalid input. */
int lz_feed(lz_ctx *ctx, const lz_input *in);

/* Takes the next output. 1 with out filled, 0 when there is nothing. */
int lz_poll(lz_ctx *ctx, lz_output *out);

/* Helpers around lz_feed. */
int lz_tick(lz_ctx *ctx);
int lz_online(lz_ctx *ctx, int up);
int lz_connected(lz_ctx *ctx, uint8_t relay, int up);
int lz_received(lz_ctx *ctx, uint8_t relay, const char *text, size_t len);
int lz_http_response(lz_ctx *ctx, uint32_t handle, int status, uint64_t date,
                     const uint8_t *body, size_t len);
int lz_http_failed(lz_ctx *ctx, uint32_t handle);
int lz_press(lz_ctx *ctx);
int lz_long_press(lz_ctx *ctx);
int lz_ask(lz_ctx *ctx, uint8_t button);

/* Sets the own reported state directly (the ESPHome component, decision
 * 12): reason, duration and whether to share it. lz_clear_state ends it. */
int lz_set_state(lz_ctx *ctx, const uint8_t *reason, size_t reason_len, uint32_t minutes,
                 int share);
int lz_clear_state(lz_ctx *ctx);

/* Factory reset: erases everything, as a removal does. */
void lz_factory_reset(lz_ctx *ctx);

/* ---- persistence ---- */

/* A writer for lz_save: stores len bytes; 0 on success. */
typedef int (*lz_write_fn)(void *user, const uint8_t *data, size_t len);

/* Streams the versioned state blob (with a SHA-256 over it at the end). */
int lz_save(const lz_ctx *ctx, lz_write_fn write, void *user);
/* The same into a buffer. */
int lz_save_buf(const lz_ctx *ctx, uint8_t *buf, size_t cap, size_t *len);
/* Upper bound of the blob with the build's limits. */
size_t lz_save_max(void);
/* Restores a blob; refuses a damaged one, another version or counts above
 * the build's limits, and leaves the context uninstalled then. */
int lz_load(lz_ctx *ctx, const uint8_t *blob, size_t len);

/* ---- what to show ---- */

typedef struct {
    uint8_t installed;
    uint8_t removed;
    uint8_t clock_warning;
    uint8_t n_open;
    uint8_t open[LZ_MAX_CONTACTS]; /* contacts with an open request */
    uint8_t n_answers;
    struct {
        uint8_t contact;
        uint8_t kind;
        uint64_t at;
        lz_reason reason;
    } answers[LZ_MAX_CONTACTS];
    uint8_t state_set;
    uint8_t state_handed; /* set on a full device */
    lz_reason state_reason;
    uint64_t state_until;
    int led;
} lz_view_t;

void lz_view(const lz_ctx *ctx, lz_view_t *v);

uint8_t lz_contact_count(const lz_ctx *ctx);
const lz_cname *lz_contact_name(const lz_ctx *ctx, uint8_t contact);
/* Earliest time the satellite may ask this contact; at most now means
 * right away, UINT64_MAX never (weekly limit 0). */
uint64_t lz_next_request_at(const lz_ctx *ctx, uint8_t contact, uint64_t now);
/* The WiFi of the setup, for the adapter. */
void lz_wifi(const lz_ctx *ctx, const uint8_t **name, size_t *name_len,
             const uint8_t **password, size_t *password_len);

#ifdef __cplusplus
}
#endif

#endif
