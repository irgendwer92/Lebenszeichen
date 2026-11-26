/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Rules for requests (wire-format.md, "Rules for requests"), as in
 * crates/core/src/rules.rs: at most one open request, the minimum interval
 * after a manual answer, the weekly limit over a sliding 7 x 24 hours. Both
 * directions use the same ledger: incoming requests with the own rules,
 * own requests with the contact's.
 */
#include <string.h>

#include "internal.h"

static uint64_t minutes(uint32_t m) { return (uint64_t)m * 60u; }

static uint64_t expiry(const lz_entry *e, const lz_rules *rules)
{
    return lz_sat_add(e->at, minutes(rules->expiry_minutes));
}

int lz_ledger_open(const lz_ledger *l, const lz_rules *rules, uint64_t now)
{
    int i;
    for (i = 0; i < l->n; i++) {
        if (!l->e[i].answered && now < expiry(&l->e[i], rules)) return i;
    }
    return -1;
}

uint64_t lz_ledger_next_allowed(const lz_ledger *l, const lz_rules *rules, uint64_t now)
{
    uint64_t open = 0, interval = 0, weekly = 0;
    uint64_t in_week[LZ_MAX_REQUESTS];
    size_t count = 0, limit = rules->weekly_limit;
    int i = lz_ledger_open(l, rules, now);
    if (i >= 0) open = expiry(&l->e[i], rules);
    if (l->has_last_answer) interval = lz_sat_add(l->last_answer, minutes(rules->min_interval_minutes));
    for (i = 0; i < l->n; i++) {
        if (now < lz_sat_add(l->e[i].at, LZ_WEEK_SECS)) in_week[count++] = l->e[i].at;
    }
    if (count >= limit) {
        if (limit == 0) {
            weekly = UINT64_MAX;
        } else {
            /* Entries are sorted: once this one leaves the window, fewer
             * than the limit remain. */
            weekly = lz_sat_add(in_week[count - limit], LZ_WEEK_SECS);
        }
    }
    return lz_max64(open, lz_max64(interval, weekly));
}

int lz_ledger_current(const lz_ledger *l, const uint8_t id[16], const lz_rules *rules,
                      uint64_t now)
{
    int i;
    for (i = 0; i < l->n; i++) {
        if (memcmp(l->e[i].id, id, 16) == 0 && now < expiry(&l->e[i], rules)) return i;
    }
    return -1;
}

int lz_ledger_find(const lz_ledger *l, const uint8_t id[16])
{
    int i;
    for (i = 0; i < l->n; i++) {
        if (memcmp(l->e[i].id, id, 16) == 0) return i;
    }
    return -1;
}

void lz_ledger_remove(lz_ledger *l, int index)
{
    if (index < 0 || index >= l->n) return;
    memmove(&l->e[index], &l->e[index + 1], (size_t)(l->n - index - 1) * sizeof l->e[0]);
    l->n--;
    memset(&l->e[l->n], 0, sizeof l->e[0]);
}

void lz_ledger_insert(lz_ledger *l, const lz_entry *e)
{
    int pos = 0, i;
    if (l->n == LZ_MAX_REQUESTS) {
        /* Full: forget the oldest answered request, which can no longer
         * be open, or else the oldest one. With a weekly limit above
         * LZ_MAX_REQUESTS this can undercount, as the record does. */
        int victim = 0;
        for (i = 0; i < l->n; i++) {
            if (l->e[i].answered) {
                victim = i;
                break;
            }
        }
        lz_ledger_remove(l, victim);
    }
    /* After every entry with the same or an earlier time. */
    while (pos < l->n && l->e[pos].at <= e->at) pos++;
    memmove(&l->e[pos + 1], &l->e[pos], (size_t)(l->n - pos) * sizeof l->e[0]);
    l->e[pos] = *e;
    l->n++;
}

void lz_ledger_record(lz_ledger *l, const uint8_t id[16], uint64_t at)
{
    lz_entry e;
    memset(&e, 0, sizeof e);
    memcpy(e.id, id, 16);
    e.at = at;
    lz_ledger_insert(l, &e);
}

int lz_ledger_answer(lz_ledger *l, const uint8_t id[16], uint64_t at)
{
    int i;
    for (i = 0; i < l->n; i++) {
        if (memcmp(l->e[i].id, id, 16) == 0 && !l->e[i].answered) {
            l->e[i].answered = 1;
            l->e[i].answered_at = at;
            if (!l->has_last_answer || at > l->last_answer) l->last_answer = at;
            l->has_last_answer = 1;
            return 1;
        }
    }
    return 0;
}

void lz_ledger_prune(lz_ledger *l, const lz_rules *rules, uint64_t now)
{
    int i = 0;
    while (i < l->n) {
        const lz_entry *e = &l->e[i];
        int in_week = now < lz_sat_add(e->at, LZ_WEEK_SECS);
        int open = !e->answered && now < expiry(e, rules);
        if (in_week || open) {
            i++;
        } else {
            lz_ledger_remove(l, i);
        }
    }
}

int lz_in_window(uint64_t sent_at, uint64_t now)
{
    return lz_sat_add(sent_at, LZ_MAX_MESSAGE_AGE) >= now &&
           sent_at <= lz_sat_add(now, LZ_MAX_CLOCK_AHEAD);
}

int lz_remember(lz_seen_entry *seen, uint8_t *n, uint8_t cap, uint64_t *floor,
                const uint8_t id[16], uint64_t sent_at, uint64_t now)
{
    uint8_t i = 0;
    while (i < *n) {
        if (now >= lz_sat_add(seen[i].at, LZ_REPLAY_MEMORY)) {
            memmove(&seen[i], &seen[i + 1], (size_t)(*n - i - 1) * sizeof seen[0]);
            (*n)--;
        } else {
            i++;
        }
    }
    for (i = 0; i < *n; i++) {
        if (memcmp(seen[i].id, id, 16) == 0) return 1;
    }
    if (*floor != 0 && sent_at <= *floor) return 1;
    if (*n == cap) {
        /* Forget the oldest and refuse anything not newer from now on,
         * so that a forgotten identifier cannot be replayed. */
        uint8_t oldest = 0;
        for (i = 1; i < *n; i++) {
            if (seen[i].at < seen[oldest].at) oldest = i;
        }
        if (seen[oldest].at > *floor) *floor = seen[oldest].at;
        memmove(&seen[oldest], &seen[oldest + 1], (size_t)(*n - oldest - 1) * sizeof seen[0]);
        (*n)--;
        if (sent_at <= *floor) return 1;
    }
    memcpy(seen[*n].id, id, 16);
    seen[*n].at = sent_at;
    (*n)++;
    return 0;
}
