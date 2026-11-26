/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * The wire format as far as the satellite needs it (docs/wire-format.md):
 * envelopes of form A and C, the plaintext header with the direction
 * marker, the bodies of types 3-6, 80-83 and 88-96, the satellite contact
 * record and the satellite setup.
 *
 * Every decoder reads only inside the given length and rejects what the
 * format does not define: truncated fields, texts that are too long or not
 * UTF-8, flags other than 0 and 1. Bytes behind a known body are ignored,
 * as in the Rust implementation, so that later versions can append fields.
 */
#ifndef LZ_WIRE_H
#define LZ_WIRE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Results of every lz_ function that can fail. */
enum {
    LZ_OK = 0,
    LZ_E_TRUNCATED = -1, /* input ends before a field */
    LZ_E_TOO_LONG = -2,  /* a value or the output exceeds its limit */
    LZ_E_UTF8 = -3,      /* text that is not UTF-8 */
    LZ_E_VALUE = -4,     /* a value the format does not define */
    LZ_E_VERSION = -5,   /* unknown version byte */
    LZ_E_DECRYPT = -6,   /* does not open, or carries the own direction */
    LZ_E_FULL = -7,      /* a table of the context is full */
    LZ_E_STATE = -8,     /* not possible in the current state */
    LZ_E_ARG = -9,       /* invalid argument */
    LZ_E_LIMIT = -10,    /* more contacts or relays than the build allows */
    LZ_E_NOT_YET = -11,  /* the rules forbid it now */
    LZ_E_CRYPTO = -12    /* the crypto backend failed */
};

#define LZ_PROTOCOL_VERSION 1
#define LZ_ENVELOPE_LEN 297
#define LZ_PLAINTEXT_LEN 256
#define LZ_HEADER_LEN 27
/* Longest body in form A: offset 255 holds the direction marker. */
#define LZ_MAX_BODY_A (LZ_PLAINTEXT_LEN - 1 - LZ_HEADER_LEN)
#define LZ_MAX_BODY_C (LZ_PLAINTEXT_LEN - LZ_HEADER_LEN)

#define LZ_MAX_URL_LEN 120
#define LZ_MAX_REASON_LEN 64
#define LZ_MAX_NAME_LEN 64
#define LZ_MAX_WIFI_NAME_LEN 32
#define LZ_MAX_WIFI_PASSWORD_LEN 64
#define LZ_MAX_PART_LEN 178
#define LZ_RECORD_MAX_LEN 698
#define LZ_RECORD_MAX_PARTS 4
#define LZ_RECENT_MAX 16
#define LZ_SETUP_VERSION 1
#define LZ_SETUP_HEADER_MAX_LEN 461

/* Times, in seconds. */
#define LZ_MAX_MESSAGE_AGE (24u * 3600u + 600u)
#define LZ_MAX_CLOCK_AHEAD 600u
#define LZ_REPLAY_MEMORY (25u * 3600u)
#define LZ_KEEP_SECS (24u * 3600u)
#define LZ_WEEK_SECS (7u * 24u * 3600u)

/* Message types. */
enum {
    LZ_T_HELLO = 1,
    LZ_T_HELLO_OK = 2,
    LZ_T_RULES = 3,
    LZ_T_REQUEST = 4,
    LZ_T_RESPONSE = 5,
    LZ_T_END = 6,
    LZ_T_REVEAL = 7,
    LZ_T_SAT_ANSWERED = 80,
    LZ_T_SAT_ASKED = 81,
    LZ_T_SAT_STATE_SHARED = 82,
    LZ_T_SAT_STATE_CLEARED = 83,
    LZ_T_HOME_CONTACT_PART = 88,
    LZ_T_HOME_RESPONDED = 89,
    LZ_T_HOME_REQUESTED = 90,
    LZ_T_HOME_RULES = 91,
    LZ_T_HOME_CONTACT_SETTINGS = 92,
    LZ_T_HOME_SETTINGS = 93,
    LZ_T_HOME_STATE_SHARED = 94,
    LZ_T_HOME_STATE_CLEARED = 95,
    LZ_T_HOME_SEND_TOKEN = 96
};

/* Kinds of a response. */
enum { LZ_KIND_MANUAL = 1, LZ_KIND_REPORTED = 2 };

typedef struct {
    uint8_t len;
    uint8_t b[LZ_MAX_REASON_LEN];
} lz_reason;

typedef struct {
    uint8_t len;
    uint8_t b[LZ_MAX_URL_LEN];
} lz_url;

typedef struct {
    uint8_t len;
    uint8_t b[LZ_MAX_NAME_LEN];
} lz_cname;

/* Sending time and identifier; newer means a later time, or the same time
 * and a larger identifier (bytewise). */
typedef struct {
    uint64_t at;
    uint8_t id[16];
} lz_stamp;

typedef struct {
    uint32_t min_interval_minutes;
    uint32_t expiry_minutes;
    uint16_t weekly_limit;
} lz_rules;

extern const lz_rules LZ_RULES_DEFAULT; /* 360, 1440, 7 */

/* The satellite's settings for one contact (home_contact_settings). */
typedef struct {
    uint8_t reported;   /* 0 or 1 */
    uint8_t ask_button; /* 0 for none */
} lz_flags;

/* home_settings. preset_minutes 0 means no preset; share and reason are
 * then 0 and empty. */
typedef struct {
    uint16_t waiting_minutes;
    uint32_t preset_minutes;
    uint8_t share;
    lz_reason reason;
} lz_settings;

extern const lz_settings LZ_SETTINGS_DEFAULT; /* 15 minutes, no preset */

/* A decoded plaintext. Bodies are parsed for the types listed in the
 * union; others (hello, reveal, messages between full devices, unknown
 * types) decode with only the header, as the satellite drops them. */
typedef struct {
    uint8_t type;
    uint8_t id[16];
    uint64_t sent_at;
    union {
        lz_rules rules; /* 3 */
        struct {
            uint8_t request[16];
            uint8_t kind;
            lz_reason reason; /* empty for manual */
        } response;           /* 5 */
        struct {
            uint8_t contact[32];
            uint8_t request[16];
            uint8_t kind;
            lz_reason reason;
            uint64_t at;
        } responded; /* 80, 89 */
        struct {
            uint8_t contact[32];
            uint8_t request[16];
            uint64_t at;
        } requested; /* 81, 90 */
        struct {
            lz_reason reason;
            uint64_t until;
        } state; /* 82, 94 */
        struct {
            uint8_t contact[32];
            uint8_t transfer[16];
            uint8_t index;
            uint8_t count;
            uint8_t len;
            uint8_t data[LZ_MAX_PART_LEN];
        } part; /* 88 */
        struct {
            uint8_t contact[32];
            lz_rules rules;
        } rules_set; /* 91 */
        struct {
            uint8_t contact[32];
            lz_flags flags;
        } contact_settings;     /* 92 */
        lz_settings settings;   /* 93 */
        uint8_t send_token[32]; /* 96 */
    } u;
} lz_message;

/* Whether the satellite parses (and can encode) the body of this type. */
int lz_type_known(uint8_t type);
/* Up types 80-83 and down types 88-96 the satellite knows. */
int lz_type_is_up(uint8_t type);
int lz_type_is_down(uint8_t type);

/* Writes the plaintext (header, body, zero padding) into out. */
int lz_message_encode(const lz_message *m, uint8_t out[LZ_PLAINTEXT_LEN]);
/* Reads a plaintext of len bytes. */
int lz_message_decode(lz_message *m, const uint8_t *plaintext, size_t len);

/* Checks that UTF-8 is valid, as Rust's str::from_utf8 does. */
int lz_utf8_valid(const uint8_t *s, size_t len);

/* Form A (crypto_box) with a shared key from lz_box_beforenm. The direction
 * marker at offset 255 is 1 if the sender's public key is the lower one.
 * lz_seal_a fails with LZ_E_TOO_LONG for a body that reaches offset 255. */
int lz_seal_a(uint8_t env[LZ_ENVELOPE_LEN], const lz_message *m, const uint8_t shared[32],
              const uint8_t own_pk[32], const uint8_t peer_pk[32], const uint8_t nonce[24]);
/* Opens and decodes; LZ_E_DECRYPT also for the own direction marker. A
 * marker of 0 (older senders) is accepted, as wire-format.md says. */
int lz_open_a(lz_message *m, const uint8_t *env, size_t len, const uint8_t shared[32],
              const uint8_t own_pk[32], const uint8_t peer_pk[32]);

/* Form C (crypto_secretbox). */
int lz_seal_c(uint8_t env[LZ_ENVELOPE_LEN], const lz_message *m, const uint8_t key[32],
              const uint8_t nonce[24]);
int lz_open_c(lz_message *m, const uint8_t *env, size_t len, const uint8_t key[32]);

/* The raw plaintext, for tests and parked messages. */
int lz_seal_plain(uint8_t env[LZ_ENVELOPE_LEN], const uint8_t plaintext[LZ_PLAINTEXT_LEN],
                  const uint8_t key[32], const uint8_t nonce[24]);
int lz_open_plain(uint8_t plaintext[LZ_PLAINTEXT_LEN], const uint8_t *env, size_t len,
                  const uint8_t key[32]);

/* The satellite contact record (wire-format.md, "Satellite contact
 * record"). An open request with an all-zero identifier means none. */
typedef struct {
    uint8_t secret_key[32];
    uint8_t peer_key[32];
    uint8_t enrolment[32];
    lz_url own_relay;
    uint8_t peer_mailbox[32];
    uint8_t peer_send_token[32];
    lz_url peer_relay;
    lz_rules own_rules;
    lz_stamp own_rules_stamp;
    lz_rules peer_rules;
    lz_stamp peer_rules_stamp;
    lz_flags flags;
    uint8_t open_id[16];
    uint64_t open_at;
    uint64_t last_answer; /* 0 for none */
    uint8_t recent_len;
    uint64_t recent[LZ_RECENT_MAX];
    lz_cname name;
} lz_contact_record;

int lz_contact_record_decode(lz_contact_record *r, const uint8_t *in, size_t len);
/* Returns the length written (at most LZ_RECORD_MAX_LEN). */
size_t lz_contact_record_encode(const lz_contact_record *r, uint8_t out[LZ_RECORD_MAX_LEN]);

/* The fixed part of the setup; the contacts follow at entries_offset. */
typedef struct {
    uint8_t reader[16];
    uint8_t wifi_name_len;
    uint8_t wifi_name[LZ_MAX_WIFI_NAME_LEN];
    uint8_t wifi_password_len;
    uint8_t wifi_password[LZ_MAX_WIFI_PASSWORD_LEN];
    lz_url relay;
    uint8_t mailbox[32];
    uint8_t enrolment[32];
    uint8_t send_token[32];
    uint8_t key[32];
    lz_stamp settings_stamp;
    lz_settings settings;
    uint8_t contacts;
    size_t entries_offset;
} lz_setup_header;

int lz_setup_header_decode(lz_setup_header *h, const uint8_t *in, size_t len);
/* The next contact entry: own mailbox (32), length (2), record. */
int lz_setup_entry_next(const uint8_t *in, size_t len, size_t *offset, uint8_t mailbox[32],
                        lz_contact_record *r);

/* Compares stamps: <0, 0, >0. */
int lz_stamp_cmp(const lz_stamp *a, const lz_stamp *b);

#ifdef __cplusplus
}
#endif

#endif
