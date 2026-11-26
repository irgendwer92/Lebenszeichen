/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* Internal interfaces of lz_core; not part of the public API. */
#ifndef LZ_INTERNAL_H
#define LZ_INTERNAL_H

#include <stddef.h>
#include <stdint.h>

#include "lz/lz.h"

/* ---- bounded reader and writer; the first error sticks ---- */

typedef struct {
    const uint8_t *p;
    size_t len;
    int err;
} lz_rd;

typedef struct {
    uint8_t *p;
    size_t cap, pos;
    int err;
} lz_wr;

void lz_rd_init(lz_rd *r, const uint8_t *p, size_t len);
const uint8_t *lz_rd_take(lz_rd *r, size_t n);
void lz_rd_bytes(lz_rd *r, uint8_t *out, size_t n);
uint8_t lz_rd_u8(lz_rd *r);
uint16_t lz_rd_u16(lz_rd *r);
uint32_t lz_rd_u32(lz_rd *r);
uint64_t lz_rd_u64(lz_rd *r);
void lz_rd_text(lz_rd *r, uint8_t *len_out, uint8_t *out, size_t max);
uint8_t lz_rd_flag(lz_rd *r);
void lz_rd_rules(lz_rd *r, lz_rules *rules);
void lz_rd_stamp(lz_rd *r, lz_stamp *s);
void lz_rd_settings(lz_rd *r, lz_settings *s);

void lz_wr_init(lz_wr *w, uint8_t *p, size_t cap);
void lz_wr_bytes(lz_wr *w, const uint8_t *b, size_t n);
void lz_wr_u8(lz_wr *w, uint8_t v);
void lz_wr_u16(lz_wr *w, uint16_t v);
void lz_wr_u32(lz_wr *w, uint32_t v);
void lz_wr_u64(lz_wr *w, uint64_t v);
void lz_wr_text(lz_wr *w, uint8_t len, const uint8_t *b);
void lz_wr_rules(lz_wr *w, const lz_rules *rules);
void lz_wr_stamp(lz_wr *w, const lz_stamp *s);
void lz_wr_settings(lz_wr *w, const lz_settings *s);

uint32_t lz_get_u32(const uint8_t *p);
uint64_t lz_get_u64(const uint8_t *p);
void lz_put_u32(uint8_t *p, uint32_t v);
void lz_put_u64(uint8_t *p, uint64_t v);

static inline int lz_is_zero(const uint8_t *p, size_t n)
{
    uint8_t d = 0;
    size_t i;
    for (i = 0; i < n; i++) d |= p[i];
    return d == 0;
}

static inline uint64_t lz_sat_add(uint64_t a, uint64_t b)
{
    return a > UINT64_MAX - b ? UINT64_MAX : a + b;
}

static inline uint64_t lz_min64(uint64_t a, uint64_t b) { return a < b ? a : b; }
static inline uint64_t lz_max64(uint64_t a, uint64_t b) { return a > b ? a : b; }

/* ---- rules (ledger.c) ---- */

uint64_t lz_ledger_next_allowed(const lz_ledger *l, const lz_rules *rules, uint64_t now);
/* Index of the open request, or -1. */
int lz_ledger_open(const lz_ledger *l, const lz_rules *rules, uint64_t now);
/* Index of a request that has not expired (answered or not), or -1. */
int lz_ledger_current(const lz_ledger *l, const uint8_t id[16], const lz_rules *rules,
                      uint64_t now);
int lz_ledger_find(const lz_ledger *l, const uint8_t id[16]);
void lz_ledger_record(lz_ledger *l, const uint8_t id[16], uint64_t at);
void lz_ledger_insert(lz_ledger *l, const lz_entry *e);
void lz_ledger_remove(lz_ledger *l, int index);
int lz_ledger_answer(lz_ledger *l, const uint8_t id[16], uint64_t at);
void lz_ledger_prune(lz_ledger *l, const lz_rules *rules, uint64_t now);

/* Replay protection with a floor: returns 1 if the message counts as seen
 * (remembered, or sent at or before the newest identifier forgotten for
 * lack of room). */
int lz_remember(lz_seen_entry *seen, uint8_t *n, uint8_t cap, uint64_t *floor,
                const uint8_t id[16], uint64_t sent_at, uint64_t now);
/* The window against replay: at most 24 h 10 min old, 10 min ahead. */
int lz_in_window(uint64_t sent_at, uint64_t now);

/* ---- the satellite (satellite.c) ---- */

int lz_rand(lz_ctx *ctx, uint8_t *buf, size_t len);
void lz_event_push(lz_ctx *ctx, int type, int contact);
lz_event *lz_event_last(lz_ctx *ctx);

void lz_sat_receive_contact(lz_ctx *ctx, int contact, const uint8_t *env, size_t len);
void lz_sat_receive_channel(lz_ctx *ctx, const uint8_t *env, size_t len);
void lz_sat_press(lz_ctx *ctx);
void lz_sat_long_press(lz_ctx *ctx);
void lz_sat_ask(lz_ctx *ctx, uint8_t button);
int lz_sat_set_state(lz_ctx *ctx, const lz_reason *reason, uint32_t minutes, int share);
int lz_sat_clear_state(lz_ctx *ctx);
void lz_sat_tick(lz_ctx *ctx);
void lz_sat_expire(lz_ctx *ctx);
/* The state that applies: 0 none, 1 the own one, 2 the handed one. */
int lz_sat_current_state(const lz_ctx *ctx, uint64_t now, lz_state_slot *out);
void lz_sat_track_open(lz_ctx *ctx);
void lz_sat_remove_contact(lz_ctx *ctx, int contact);
int lz_sat_contact_keys(lz_ctx *ctx, int contact);
int lz_sat_relay_index(lz_ctx *ctx, const lz_url *url, int add);
int lz_sat_contact_index(const lz_ctx *ctx, const uint8_t mailbox[32]);
void lz_sat_wipe(lz_ctx *ctx);
int lz_sat_add_contact(lz_ctx *ctx, const uint8_t mailbox[32], const lz_contact_record *r);

#endif
