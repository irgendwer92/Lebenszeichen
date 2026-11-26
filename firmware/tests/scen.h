/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* The world of the scenario tests: the core, the fake relay, the contacts
 * (whose side the test plays) and the channel (the phone's side). */
#ifndef LZ_SCEN_H
#define LZ_SCEN_H

#include "internal.h"
#include "test.h"

#define T0 1794600000ull

typedef struct {
    uint8_t own_mailbox[32], enrol[32], own_send[32];
    uint8_t sat_sk[32], sat_pk[32], peer_sk[32], peer_pk[32], k[32];
    uint8_t peer_mailbox[32], peer_send[32];
    char name[16];
    char own_relay[40]; /* empty: the channel's relay */
} scontact;

typedef struct {
    t_world w;
    lz_ctx ctx;
    fake_relay fr;
    uint8_t key[32], mailbox[32], enrol[32], send[32];
    scontact c[4];
    int n;
    t_rng rng;
} scen;

extern scen S;

void scen_init(scen *s, int n, const lz_settings *settings);
void scen_keys(scen *s, int i, const char *name);
void scen_record(const scen *s, int i, lz_contact_record *r);
size_t scen_setup(scen *s, uint8_t *out, size_t cap, const lz_settings *settings);
void scen_connect(scen *s);
void scen_advance(scen *s, uint64_t secs);
void scen_restart(scen *s);
void contact_sends(scen *s, int i, lz_message *m);
void phone_seal(scen *s, lz_message *m, uint8_t env[LZ_ENVELOPE_LEN]);
void phone_sends(scen *s, lz_message *m);
int peer_got(scen *s, int i, int *from, lz_message *out);
int up_got(scen *s, int *from, lz_message *out, int type);
lz_message msg(uint8_t type);
lz_message request_from(scen *s, int i);
void set_flags(scen *s, int i, int reported, int button);
void set_settings(scen *s, uint16_t waiting, uint32_t preset, int share, const char *reason);

#define lz_is_zero_test(p) lz_is_zero((p), sizeof(p))
#define lz_put_u64_test lz_put_u64

#endif
