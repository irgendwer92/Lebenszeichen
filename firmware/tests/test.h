/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* A minimal test harness and the helpers the host tests share. */
#ifndef LZ_TEST_H
#define LZ_TEST_H

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "lz/lz.h"

extern int t_failures;
extern int t_checks;

#define CHECK(cond)                                                                    \
    do {                                                                               \
        t_checks++;                                                                    \
        if (!(cond)) {                                                                 \
            fprintf(stderr, "%s:%d: CHECK(%s) failed\n", __FILE__, __LINE__, #cond); \
            t_failures++;                                                              \
        }                                                                              \
    } while (0)

#define CHECK_EQ(a, b)                                                                  \
    do {                                                                                \
        long long a_ = (long long)(a), b_ = (long long)(b);                             \
        t_checks++;                                                                     \
        if (a_ != b_) {                                                                 \
            fprintf(stderr, "%s:%d: %s == %s failed: %lld != %lld\n", __FILE__, __LINE__, \
                    #a, #b, a_, b_);                                                    \
            t_failures++;                                                               \
        }                                                                               \
    } while (0)

#define CHECK_MEM(a, b, n)                                                                 \
    do {                                                                                   \
        t_checks++;                                                                        \
        if (memcmp((a), (b), (n)) != 0) {                                                  \
            fprintf(stderr, "%s:%d: memory %s != %s\n", __FILE__, __LINE__, #a, #b);       \
            t_failures++;                                                                  \
        }                                                                                  \
    } while (0)

/* Suites. */
void suite_crypto(void);
void suite_wire(void);
void suite_relay(void);
void suite_store(void);
void suite_vectors(const char *path);
void suite_scenarios(void);
void suite_fuzz(void);

/* ---- helpers.c ---- */

/* Hex into bytes; returns the number of bytes, or -1. */
int t_unhex(uint8_t *out, size_t cap, const char *hex, size_t len);

/* The reproducible randomness of the Rust test vectors: SHA-256 of
 * ("lebenszeichen-test-vectors" || seed), then SHA-256(seed || counter). */
typedef struct {
    uint8_t seed[32];
    uint64_t counter;
    uint8_t buf[32];
    size_t left;
} t_rng;

void t_rng_init(t_rng *r, const uint8_t *seed, size_t len);
void t_rng_fill(t_rng *r, uint8_t *out, size_t len);

/* A platform with a settable clock and t_rng. */
typedef struct {
    uint64_t now;
    t_rng rng;
    int fail_random;
} t_world;

void t_platform(lz_platform *pf, t_world *w);

/* A test-only JSON reader over a whole file (allocates; never in the core). */
typedef struct t_json t_json;
t_json *t_json_parse(const char *text, size_t len);
void t_json_free(t_json *j);
const t_json *t_json_get(const t_json *obj, const char *key);
size_t t_json_len(const t_json *arr);
const t_json *t_json_at(const t_json *arr, size_t i);
const char *t_json_str(const t_json *v, size_t *len);
long long t_json_num(const t_json *v);
int t_json_is_null(const t_json *v);
char *t_read_file(const char *path, size_t *len);

/* A setup header encoder (the core only decodes setups). */
size_t t_setup_header(uint8_t *out, size_t cap, const lz_setup_header *h);
size_t t_setup_entry(uint8_t *out, size_t cap, const uint8_t mailbox[32],
                     const lz_contact_record *r);

/* ---- fakerelay.c: a relay that speaks the satellite's HTTP and frames ---- */

#define FR_MAX_MAILBOXES 24
#define FR_MAX_MESSAGES 48
#define FR_MAX_SENT 64

typedef struct {
    int exists;
    uint8_t id[32];
    char relay[128];
    uint8_t send_hash[32];
    int has_reader;     /* the satellite's reader is registered */
    uint8_t reader_hash[32];
    uint8_t msgs[FR_MAX_MESSAGES][LZ_ENVELOPE_LEN];
    int nmsgs;
    uint64_t seq0;      /* seq of msgs[0] */
    uint64_t acked;
    int subscribed;     /* on the live connection */
    int sat_relay;      /* the core's relay index, -1 if not read by it */
} fr_mailbox;

typedef struct {
    uint8_t env[LZ_ENVELOPE_LEN];
    uint8_t mailbox[32];
    uint64_t at;
} fr_sent;

typedef struct {
    t_world *world;
    lz_ctx *ctx;
    fr_mailbox mb[FR_MAX_MAILBOXES];
    int nmb;
    int connected[LZ_MAX_RELAYS];
    int online;           /* answers HTTP and connections */
    int64_t date_offset;  /* the relay's clock against the world's */
    int reader_tokens;
    fr_sent sent[FR_MAX_SENT]; /* every accepted POST */
    int nsent;
    int persists, erases, views;
    int led;
    lz_event events[64];
    int nevents;
    uint8_t blob[200000];
    size_t blob_len;
    int http_requests;
    int frames_out;
    char last_frame[700];
} fake_relay;

void fr_init(fake_relay *fr, t_world *w, lz_ctx *ctx);
fr_mailbox *fr_add(fake_relay *fr, const char *relay, const uint8_t id[32],
                   const uint8_t send_token[32], const uint8_t *reader_token);
fr_mailbox *fr_find(fake_relay *fr, const uint8_t id[32]);
/* Puts an envelope into a mailbox; delivered at once if subscribed. */
void fr_put(fake_relay *fr, const uint8_t mailbox[32], const uint8_t env[LZ_ENVELOPE_LEN]);
/* Deletes a mailbox, or the satellite's reader on it. */
void fr_delete(fake_relay *fr, const uint8_t mailbox[32]);
void fr_revoke(fake_relay *fr, const uint8_t mailbox[32]);
/* Drains lz_poll and plays the relay's part, until nothing is left. */
void fr_run(fake_relay *fr);
/* Counts events of a type since the last fr_clear_events. */
int fr_events(const fake_relay *fr, int type);
void fr_clear_events(fake_relay *fr);
/* Sent envelopes to a mailbox since index from. */
int fr_sent_to(const fake_relay *fr, const uint8_t mailbox[32], int from, int *last);

#endif
