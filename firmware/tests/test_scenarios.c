/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Scenarios of crates/client/src/tests/satellite.rs, played against the
 * fake relay: the test is the contact and the phone, the core is driven
 * only through lz_feed and lz_poll.
 */
#include <stdlib.h>

#include "test.h"
#include "scen.h"

scen S;

static const char RELAY[] = "https://relay.example";
static const char OTHER[] = "https://other.example";

static void url(lz_url *u, const char *s)
{
    u->len = (uint8_t)strlen(s);
    memcpy(u->b, s, u->len);
}

void scen_record(const scen *s, int i, lz_contact_record *r)
{
    const scontact *c = &s->c[i];
    memset(r, 0, sizeof *r);
    memcpy(r->secret_key, c->sat_sk, 32);
    memcpy(r->peer_key, c->peer_pk, 32);
    memcpy(r->enrolment, c->enrol, 32);
    url(&r->own_relay, c->own_relay[0] ? c->own_relay : RELAY);
    memcpy(r->peer_mailbox, c->peer_mailbox, 32);
    memcpy(r->peer_send_token, c->peer_send, 32);
    url(&r->peer_relay, OTHER);
    r->own_rules = LZ_RULES_DEFAULT;
    r->own_rules_stamp.at = T0 - 1000;
    memset(r->own_rules_stamp.id, 0x36, 16);
    r->peer_rules = LZ_RULES_DEFAULT;
    r->name.len = (uint8_t)strlen(c->name);
    memcpy(r->name.b, c->name, r->name.len);
}

void scen_keys(scen *s, int i, const char *name)
{
    scontact *c = &s->c[i];
    memset(c, 0, sizeof *c);
    memset(c->own_mailbox, 0x10 + i, 32);
    memset(c->enrol, 0x20 + i, 32);
    memset(c->own_send, 0x28 + i, 32);
    memset(c->sat_sk, 0x50 + i, 32);
    memset(c->peer_sk, 0x60 + i, 32);
    memset(c->peer_mailbox, 0x70 + i, 32);
    memset(c->peer_send, 0x78 + i, 32);
    lz_scalarmult_base(c->sat_pk, c->sat_sk);
    lz_scalarmult_base(c->peer_pk, c->peer_sk);
    CHECK(lz_box_beforenm(c->k, c->sat_pk, c->peer_sk) == 0);
    snprintf(c->name, sizeof c->name, "%s", name);
}

size_t scen_setup(scen *s, uint8_t *out, size_t cap, const lz_settings *settings)
{
    lz_setup_header h;
    size_t len;
    int i;
    memset(&h, 0, sizeof h);
    memset(h.reader, 0x66, 16);
    h.wifi_name_len = 4;
    memcpy(h.wifi_name, "home", 4);
    h.wifi_password_len = 6;
    memcpy(h.wifi_password, "secret", 6);
    url(&h.relay, RELAY);
    memcpy(h.mailbox, s->mailbox, 32);
    memcpy(h.enrolment, s->enrol, 32);
    memcpy(h.send_token, s->send, 32);
    memcpy(h.key, s->key, 32);
    h.settings = settings ? *settings : LZ_SETTINGS_DEFAULT;
    h.contacts = (uint8_t)s->n;
    len = t_setup_header(out, cap, &h);
    for (i = 0; i < s->n; i++) {
        lz_contact_record r;
        scen_record(s, i, &r);
        len += t_setup_entry(out + len, cap - len, s->c[i].own_mailbox, &r);
    }
    return len;
}

void scen_connect(scen *s)
{
    lz_online(&s->ctx, 1);
    fr_run(&s->fr);
    lz_tick(&s->ctx);
    fr_run(&s->fr);
}

void scen_init(scen *s, int n, const lz_settings *settings)
{
    static const char *names[] = {"Bert", "Carl", "Dora", "Emil"};
    uint8_t setup[4096];
    lz_platform pf;
    size_t len;
    int i;
    memset(s, 0, sizeof *s);
    s->w.now = T0;
    t_rng_init(&s->w.rng, (const uint8_t *)"satellite", 9);
    t_rng_init(&s->rng, (const uint8_t *)"world", 5);
    memset(s->key, 0x41, 32);
    memset(s->mailbox, 0x42, 32);
    memset(s->enrol, 0x43, 32);
    memset(s->send, 0x44, 32);
    s->n = n;
    for (i = 0; i < n; i++) scen_keys(s, i, names[i]);
    len = scen_setup(s, setup, sizeof setup, settings);
    t_platform(&pf, &s->w);
    CHECK(lz_init(&s->ctx, &pf) == LZ_OK);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_OK);
    fr_init(&s->fr, &s->w, &s->ctx);
    fr_add(&s->fr, RELAY, s->mailbox, s->send, s->enrol);
    for (i = 0; i < n; i++) {
        fr_add(&s->fr, RELAY, s->c[i].own_mailbox, s->c[i].own_send, s->c[i].enrol);
        fr_add(&s->fr, OTHER, s->c[i].peer_mailbox, s->c[i].peer_send, NULL);
    }
    scen_connect(s);
}

void scen_advance(scen *s, uint64_t secs)
{
    s->w.now += secs;
    lz_tick(&s->ctx);
    fr_run(&s->fr);
}

void scen_restart(scen *s)
{
    lz_platform pf;
    int i;
    CHECK(s->fr.blob_len > 0);
    t_platform(&pf, &s->w);
    CHECK(lz_init(&s->ctx, &pf) == LZ_OK);
    CHECK(lz_load(&s->ctx, s->fr.blob, s->fr.blob_len) == LZ_OK);
    memset(s->fr.connected, 0, sizeof s->fr.connected);
    for (i = 0; i < s->fr.nmb; i++) s->fr.mb[i].subscribed = 0;
    scen_connect(s);
}

static void seal_from_contact(scen *s, int i, lz_message *m, uint8_t env[LZ_ENVELOPE_LEN])
{
    uint8_t nonce[24];
    if (lz_is_zero_test(m->id)) t_rng_fill(&s->rng, m->id, 16);
    if (m->sent_at == 0) m->sent_at = s->w.now;
    t_rng_fill(&s->rng, nonce, 24);
    CHECK(lz_seal_a(env, m, s->c[i].k, s->c[i].peer_pk, s->c[i].sat_pk, nonce) == LZ_OK);
}

void contact_sends(scen *s, int i, lz_message *m)
{
    uint8_t env[LZ_ENVELOPE_LEN];
    seal_from_contact(s, i, m, env);
    fr_put(&s->fr, s->c[i].own_mailbox, env);
    fr_run(&s->fr);
}

void phone_seal(scen *s, lz_message *m, uint8_t env[LZ_ENVELOPE_LEN])
{
    uint8_t nonce[24];
    if (lz_is_zero_test(m->id)) t_rng_fill(&s->rng, m->id, 16);
    if (m->sent_at == 0) m->sent_at = s->w.now;
    t_rng_fill(&s->rng, nonce, 24);
    CHECK(lz_seal_c(env, m, s->key, nonce) == LZ_OK);
}

void phone_sends(scen *s, lz_message *m)
{
    uint8_t env[LZ_ENVELOPE_LEN];
    phone_seal(s, m, env);
    fr_put(&s->fr, s->mailbox, env);
    fr_run(&s->fr);
}

int peer_got(scen *s, int i, int *from, lz_message *out)
{
    int n = 0, k;
    for (k = *from; k < s->fr.nsent; k++) {
        if (memcmp(s->fr.sent[k].mailbox, s->c[i].peer_mailbox, 32) != 0) continue;
        CHECK(lz_open_a(out, s->fr.sent[k].env, LZ_ENVELOPE_LEN, s->c[i].k, s->c[i].peer_pk,
                        s->c[i].sat_pk) == LZ_OK);
        n++;
    }
    *from = s->fr.nsent;
    return n;
}

int up_got(scen *s, int *from, lz_message *out, int type)
{
    int n = 0, k;
    for (k = *from; k < s->fr.nsent; k++) {
        lz_message m;
        if (memcmp(s->fr.sent[k].mailbox, s->mailbox, 32) != 0) continue;
        CHECK(lz_open_c(&m, s->fr.sent[k].env, LZ_ENVELOPE_LEN, s->key) == LZ_OK);
        CHECK(lz_type_is_up(m.type));
        if (type == 0 || m.type == type) {
            *out = m;
            n++;
        }
    }
    *from = s->fr.nsent;
    return n;
}

lz_message msg(uint8_t type)
{
    lz_message m;
    memset(&m, 0, sizeof m);
    m.type = type;
    return m;
}

lz_message request_from(scen *s, int i)
{
    lz_message m = msg(LZ_T_REQUEST);
    contact_sends(s, i, &m);
    return m;
}

void set_flags(scen *s, int i, int reported, int button)
{
    lz_message m = msg(LZ_T_HOME_CONTACT_SETTINGS);
    memcpy(m.u.contact_settings.contact, s->c[i].own_mailbox, 32);
    m.u.contact_settings.flags.reported = (uint8_t)reported;
    m.u.contact_settings.flags.ask_button = (uint8_t)button;
    phone_sends(s, &m);
}

void set_settings(scen *s, uint16_t waiting, uint32_t preset, int share, const char *reason)
{
    lz_message m = msg(LZ_T_HOME_SETTINGS);
    m.u.settings.waiting_minutes = waiting;
    m.u.settings.preset_minutes = preset;
    m.u.settings.share = (uint8_t)share;
    m.u.settings.reason.len = (uint8_t)strlen(reason);
    memcpy(m.u.settings.reason.b, reason, m.u.settings.reason.len);
    phone_sends(s, &m);
}

static int is_open(const scen *s, int i)
{
    lz_view_t v;
    int k;
    lz_view(&s->ctx, &v);
    for (k = 0; k < v.n_open; k++) {
        if (v.open[k] == i) return 1;
    }
    return 0;
}

/* ---- scenarios ---- */

static void adding_takes_own_tokens(void)
{
    scen *s = &S;
    uint8_t h[32];
    int i;
    scen_init(s, 2, NULL);
    CHECK(s->ctx.channel.has_own_token);
    CHECK(!s->ctx.channel.has_new_token);
    lz_sha256(h, s->ctx.channel.own_token, 32);
    CHECK_MEM(fr_find(&s->fr, s->mailbox)->reader_hash, h, 32);
    CHECK(fr_find(&s->fr, s->mailbox)->subscribed);
    for (i = 0; i < 2; i++) {
        fr_mailbox *m = fr_find(&s->fr, s->c[i].own_mailbox);
        CHECK(s->ctx.contacts[i].has_own_token);
        lz_sha256(h, s->ctx.contacts[i].own_token, 32);
        CHECK_MEM(m->reader_hash, h, 32);
        lz_sha256(h, s->c[i].enrol, 32);
        CHECK(memcmp(m->reader_hash, h, 32) != 0);
        CHECK(m->subscribed);
    }
    CHECK(s->fr.persists > 0);
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
    /* The relay names the feature; nothing was refused. */
    CHECK_EQ(fr_events(&s->fr, LZ_EV_NO_READER_TOKENS), 0);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ACCESS_LOST), 0);
}

static void a_press_answers_every_open_request(void)
{
    scen *s = &S;
    lz_message r0, r1, got;
    int from = 0, ufrom = 0;
    scen_init(s, 2, NULL);
    fr_clear_events(&s->fr);
    r0 = request_from(s, 0);
    r1 = request_from(s, 1);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_REQUEST_OPENED), 2);
    CHECK_EQ(s->fr.led, LZ_LED_REQUEST);
    CHECK(is_open(s, 0) && is_open(s, 1));
    from = s->fr.nsent;
    ufrom = s->fr.nsent;
    s->w.now += 30;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    {
        int f = from;
        CHECK_EQ(peer_got(s, 0, &f, &got), 1);
        CHECK_EQ(got.type, LZ_T_RESPONSE);
        CHECK_EQ(got.u.response.kind, LZ_KIND_MANUAL);
        CHECK_MEM(got.u.response.request, r0.id, 16);
        CHECK_EQ(got.sent_at, s->w.now);
        f = from;
        CHECK_EQ(peer_got(s, 1, &f, &got), 1);
        CHECK_MEM(got.u.response.request, r1.id, 16);
    }
    CHECK_EQ(up_got(s, &ufrom, &got, LZ_T_SAT_ANSWERED), 2);
    CHECK_MEM(got.u.responded.contact, s->c[1].own_mailbox, 32);
    CHECK_MEM(got.u.responded.request, r1.id, 16);
    CHECK_EQ(got.u.responded.kind, LZ_KIND_MANUAL);
    CHECK_EQ(got.u.responded.at, s->w.now);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_REQUEST_CLOSED), 2);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ANSWERED), 2);
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
    /* A second press has nothing to answer. */
    from = s->fr.nsent;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(s->fr.nsent, from);
    CHECK_EQ(s->ctx.noutbox, 0);
}

static void an_answer_elsewhere_turns_the_light_off(void)
{
    scen *s = &S;
    lz_message r, m;
    int from;
    scen_init(s, 1, NULL);
    r = request_from(s, 0);
    CHECK_EQ(s->fr.led, LZ_LED_REQUEST);
    /* Reported elsewhere: the light stays. */
    m = msg(LZ_T_HOME_RESPONDED);
    memcpy(m.u.responded.contact, s->c[0].own_mailbox, 32);
    memcpy(m.u.responded.request, r.id, 16);
    m.u.responded.kind = LZ_KIND_REPORTED;
    m.u.responded.reason.len = 1;
    m.u.responded.reason.b[0] = 'x';
    m.u.responded.at = s->w.now;
    phone_sends(s, &m);
    CHECK_EQ(s->fr.led, LZ_LED_REQUEST);
    CHECK(s->ctx.contacts[0].incoming.e[0].reported);
    /* Manual elsewhere: off, and a press sends nothing. */
    m = msg(LZ_T_HOME_RESPONDED);
    memcpy(m.u.responded.contact, s->c[0].own_mailbox, 32);
    memcpy(m.u.responded.request, r.id, 16);
    m.u.responded.kind = LZ_KIND_MANUAL;
    m.u.responded.at = s->w.now;
    phone_sends(s, &m);
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
    CHECK(!is_open(s, 0));
    from = s->fr.nsent;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(s->fr.nsent, from);
    /* The minimum interval started: the next request is discarded. */
    scen_advance(s, 3600);
    request_from(s, 0);
    CHECK(!is_open(s, 0));
}

static void requests_made_elsewhere_reach_the_satellite(void)
{
    scen *s = &S;
    lz_message m, resp;
    uint8_t request[16];
    scen_init(s, 1, NULL);
    set_flags(s, 0, 0, 1);
    m = msg(LZ_T_HOME_REQUESTED);
    memcpy(m.u.requested.contact, s->c[0].own_mailbox, 32);
    memset(request, 0x99, 16);
    memcpy(m.u.requested.request, request, 16);
    m.u.requested.at = s->w.now;
    phone_sends(s, &m);
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    CHECK_EQ(s->fr.events[0].at, s->w.now + 1440 * 60);
    /* The contact's answer to it is shown. */
    resp = msg(LZ_T_RESPONSE);
    memcpy(resp.u.response.request, request, 16);
    resp.u.response.kind = LZ_KIND_MANUAL;
    scen_advance(s, 120);
    contact_sends(s, 0, &resp);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ANSWER), 1);
    CHECK_EQ(s->fr.led, LZ_LED_ANSWER_MANUAL);
    {
        lz_view_t v;
        lz_view(&s->ctx, &v);
        CHECK_EQ(v.n_answers, 1);
        CHECK_EQ(v.answers[0].kind, LZ_KIND_MANUAL);
    }
    /* A press dismisses it. */
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
}

static void asking_obeys_the_rules_of_the_contact(void)
{
    scen *s = &S;
    lz_message got, resp, rules;
    int from, ufrom;
    uint64_t asked_at, answered_at;
    scen_init(s, 1, NULL);
    set_flags(s, 0, 0, 1);
    from = ufrom = s->fr.nsent;
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    asked_at = s->w.now;
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASKED), 1);
    CHECK_EQ(peer_got(s, 0, &from, &got), 1);
    CHECK_EQ(got.type, LZ_T_REQUEST);
    {
        lz_message up;
        CHECK_EQ(up_got(s, &ufrom, &up, LZ_T_SAT_ASKED), 1);
        CHECK_MEM(up.u.requested.request, got.id, 16);
        CHECK_MEM(up.u.requested.contact, s->c[0].own_mailbox, 32);
        CHECK_EQ(up.u.requested.at, asked_at);
    }
    /* Open: refused until it expires. */
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    CHECK_EQ(s->fr.events[0].at, asked_at + 1440 * 60);
    /* A reported answer, then a manual one. */
    resp = msg(LZ_T_RESPONSE);
    memcpy(resp.u.response.request, got.id, 16);
    resp.u.response.kind = LZ_KIND_REPORTED;
    resp.u.response.reason.len = 4;
    memcpy(resp.u.response.reason.b, "busy", 4);
    scen_advance(s, 60);
    contact_sends(s, 0, &resp);
    CHECK_EQ(s->fr.led, LZ_LED_ANSWER_REPORTED);
    /* A second reported one is not accepted. */
    fr_clear_events(&s->fr);
    resp = msg(LZ_T_RESPONSE);
    memcpy(resp.u.response.request, got.id, 16);
    resp.u.response.kind = LZ_KIND_REPORTED;
    contact_sends(s, 0, &resp);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ANSWER), 0);
    resp = msg(LZ_T_RESPONSE);
    memcpy(resp.u.response.request, got.id, 16);
    resp.u.response.kind = LZ_KIND_MANUAL;
    scen_advance(s, 60);
    answered_at = s->w.now;
    contact_sends(s, 0, &resp);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ANSWER), 1);
    CHECK_EQ(s->fr.led, LZ_LED_ANSWER_MANUAL);
    /* The minimum interval runs from the manual answer. */
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    CHECK_EQ(s->fr.events[0].at, answered_at + 360 * 60);
    /* A new weekly limit of 2 from the contact. */
    rules = msg(LZ_T_RULES);
    rules.u.rules.min_interval_minutes = 0;
    rules.u.rules.expiry_minutes = 60;
    rules.u.rules.weekly_limit = 2;
    contact_sends(s, 0, &rules);
    CHECK_EQ(s->ctx.contacts[0].peer_rules.weekly_limit, 2);
    scen_advance(s, 2 * 3600);
    from = s->fr.nsent;
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(peer_got(s, 0, &from, &got), 1);
    scen_advance(s, 2 * 3600);
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    CHECK_EQ(s->fr.events[0].at, asked_at + LZ_WEEK_SECS);
    /* No contact on button 2. */
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 2);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    CHECK(s->fr.events[0].at == UINT64_MAX);
    CHECK_EQ(s->fr.events[0].contact, 0xFF);
}

static void reported_answers_after_the_waiting_time_only_where_allowed_once(void)
{
    scen *s = &S;
    lz_message got;
    int f0, f1;
    scen_init(s, 2, NULL);
    set_settings(s, 15, 480, 0, "asleep");
    set_flags(s, 0, 1, 0);
    lz_long_press(&s->ctx);
    fr_run(&s->fr);
    {
        lz_view_t v;
        lz_view(&s->ctx, &v);
        CHECK(v.state_set && !v.state_handed);
        CHECK_EQ(v.state_until, s->w.now + 480 * 60);
    }
    request_from(s, 0);
    request_from(s, 1);
    f0 = f1 = s->fr.nsent;
    scen_advance(s, 14 * 60);
    CHECK_EQ(peer_got(s, 0, &f0, &got), 0);
    scen_advance(s, 2 * 60);
    f1 = f0 = 0;
    {
        int k, n0 = 0, n1 = 0;
        for (k = 0; k < s->fr.nsent; k++) {
            if (memcmp(s->fr.sent[k].mailbox, s->c[0].peer_mailbox, 32) == 0) n0++;
            if (memcmp(s->fr.sent[k].mailbox, s->c[1].peer_mailbox, 32) == 0) n1++;
        }
        CHECK_EQ(n0, 1);
        CHECK_EQ(n1, 0);
    }
    CHECK_EQ(peer_got(s, 0, &f0, &got), 1);
    CHECK_EQ(got.u.response.kind, LZ_KIND_REPORTED);
    CHECK_EQ(got.u.response.reason.len, 6);
    CHECK_MEM(got.u.response.reason.b, "asleep", 6);
    {
        lz_message up;
        int uf = 0;
        CHECK_EQ(up_got(s, &uf, &up, LZ_T_SAT_ANSWERED), 1);
        CHECK_EQ(up.u.responded.kind, LZ_KIND_REPORTED);
    }
    /* Once per request, also after more ticks; the request stays open. */
    scen_advance(s, 10 * 60);
    CHECK_EQ(peer_got(s, 0, &f0, &got), 0);
    CHECK(is_open(s, 0));
    /* A later press still sends a manual answer to both. */
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(peer_got(s, 0, &f0, &got), 1);
    CHECK_EQ(got.u.response.kind, LZ_KIND_MANUAL);
    CHECK_EQ(peer_got(s, 1, &f1, &got), 1);
    (void)f1;
}

static void a_restart_during_the_waiting_time_still_reports_once(void)
{
    scen *s = &S;
    lz_message got;
    int f;
    scen_init(s, 1, NULL);
    set_settings(s, 15, 480, 0, "asleep");
    set_flags(s, 0, 1, 0);
    lz_long_press(&s->ctx);
    fr_run(&s->fr);
    request_from(s, 0);
    f = s->fr.nsent;
    scen_advance(s, 5 * 60);
    scen_restart(s);
    scen_advance(s, 9 * 60);
    CHECK_EQ(peer_got(s, 0, &f, &got), 0);
    scen_restart(s);
    scen_advance(s, 60 + 1);
    CHECK_EQ(peer_got(s, 0, &f, &got), 1);
    CHECK_EQ(got.u.response.kind, LZ_KIND_REPORTED);
    scen_restart(s);
    scen_advance(s, 60);
    CHECK_EQ(peer_got(s, 0, &f, &got), 0);
    /* The request is shown again after a restart. */
    CHECK(is_open(s, 0));
    CHECK_EQ(s->fr.led, LZ_LED_REQUEST);
}

static void states_are_shared_both_ways(void)
{
    scen *s = &S;
    lz_message got, m;
    lz_state_slot st;
    int uf;
    scen_init(s, 1, NULL);
    set_settings(s, 15, 60, 1, "asleep");
    uf = s->fr.nsent;
    lz_long_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(up_got(s, &uf, &got, LZ_T_SAT_STATE_SHARED), 1);
    CHECK_EQ(got.u.state.until, s->w.now + 3600);
    CHECK_MEM(got.u.state.reason.b, "asleep", 6);
    CHECK_EQ(got.sent_at, s->w.now);

    /* Down: "Phone empty" replaces it; the shared own state goes. */
    scen_advance(s, 10);
    m = msg(LZ_T_HOME_STATE_SHARED);
    m.u.state.reason.len = 11;
    memcpy(m.u.state.reason.b, "Phone empty", 11);
    m.u.state.until = s->w.now + 7200;
    phone_sends(s, &m);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 2);
    CHECK_MEM(st.reason.b, "Phone empty", 11);
    CHECK(!s->ctx.own_state.has);
    /* A clear ends it. */
    scen_advance(s, 10);
    m = msg(LZ_T_HOME_STATE_CLEARED);
    phone_sends(s, &m);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 0);

    /* An own state that was never shared stays through a clear. */
    set_settings(s, 15, 60, 0, "out");
    lz_long_press(&s->ctx);
    fr_run(&s->fr);
    scen_advance(s, 10);
    m = msg(LZ_T_HOME_STATE_SHARED);
    m.u.state.reason.len = 11;
    memcpy(m.u.state.reason.b, "Phone empty", 11);
    m.u.state.until = s->w.now + 7200;
    phone_sends(s, &m);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 2);
    scen_advance(s, 10);
    m = msg(LZ_T_HOME_STATE_CLEARED);
    phone_sends(s, &m);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 1);
    CHECK_MEM(st.reason.b, "out", 3);
    /* An older clear is outdated. */
    m = msg(LZ_T_HOME_STATE_SHARED);
    m.sent_at = s->w.now - 100;
    m.u.state.reason.len = 1;
    m.u.state.reason.b[0] = 'z';
    m.u.state.until = s->w.now + 7200;
    phone_sends(s, &m);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 1);

    /* A clear from the satellite ends the shared state everywhere. */
    set_settings(s, 15, 60, 1, "asleep");
    lz_long_press(&s->ctx); /* ends "out", never shared: nothing up */
    fr_run(&s->fr);
    uf = s->fr.nsent;
    scen_advance(s, 10);
    lz_long_press(&s->ctx); /* sets and shares "asleep" */
    fr_run(&s->fr);
    scen_advance(s, 10);
    lz_long_press(&s->ctx); /* ends it */
    fr_run(&s->fr);
    CHECK_EQ(up_got(s, &uf, &got, 0), 2);
    CHECK_EQ(got.type, LZ_T_SAT_STATE_CLEARED);
    CHECK_EQ(lz_sat_current_state(&s->ctx, s->w.now, &st), 0);
    /* The ESPHome path: a state set directly. */
    uf = s->fr.nsent;
    CHECK(lz_set_state(&s->ctx, (const uint8_t *)"sensor", 6, 30, 1) == LZ_OK);
    fr_run(&s->fr);
    CHECK_EQ(up_got(s, &uf, &got, LZ_T_SAT_STATE_SHARED), 1);
    CHECK(lz_clear_state(&s->ctx) == LZ_OK);
    fr_run(&s->fr);
    CHECK_EQ(up_got(s, &uf, &got, LZ_T_SAT_STATE_CLEARED), 1);
    CHECK(lz_clear_state(&s->ctx) == LZ_E_STATE);
    CHECK(lz_set_state(&s->ctx, (const uint8_t *)"\xff", 1, 30, 0) == LZ_E_ARG);
    CHECK(lz_set_state(&s->ctx, (const uint8_t *)"x", 1, 0, 0) == LZ_E_ARG);
}

static void settings_and_send_token_are_applied_in_order(void)
{
    scen *s = &S;
    lz_message m, got;
    uint8_t token1[32], token2[32];
    fr_mailbox *ch;
    int uf;
    scen_init(s, 1, NULL);
    set_settings(s, 20, 0, 0, "");
    CHECK_EQ(s->ctx.settings.waiting_minutes, 20);
    /* An older one is dropped. */
    m = msg(LZ_T_HOME_SETTINGS);
    m.sent_at = s->w.now - 60;
    m.u.settings.waiting_minutes = 99;
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.settings.waiting_minutes, 20);
    set_flags(s, 0, 1, 3);
    m = msg(LZ_T_HOME_CONTACT_SETTINGS);
    m.sent_at = s->w.now - 60;
    memcpy(m.u.contact_settings.contact, s->c[0].own_mailbox, 32);
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.contacts[0].flags.ask_button, 3);
    CHECK_EQ(s->ctx.contacts[0].flags.reported, 1);

    /* The send token: the relay refuses the old one; a message waits. */
    memset(token1, 0xA1, 32);
    memset(token2, 0xA2, 32);
    ch = fr_find(&s->fr, s->mailbox);
    lz_sha256(ch->send_hash, token1, 32);
    uf = s->fr.nsent;
    lz_ask(&s->ctx, 3);
    fr_run(&s->fr);
    CHECK_EQ(up_got(s, &uf, &got, LZ_T_SAT_ASKED), 0);
    CHECK_EQ(s->ctx.noutbox, 1);
    /* The last token received wins, whatever its stamp. */
    m = msg(LZ_T_HOME_SEND_TOKEN);
    memcpy(m.u.send_token, token1, 32);
    phone_sends(s, &m);
    m = msg(LZ_T_HOME_SEND_TOKEN);
    m.sent_at = s->w.now - 300;
    memcpy(m.u.send_token, token2, 32);
    phone_sends(s, &m);
    CHECK_MEM(s->ctx.channel.send_token, token2, 32);
    lz_sha256(ch->send_hash, token2, 32);
    scen_advance(s, 400);
    CHECK_EQ(up_got(s, &uf, &got, LZ_T_SAT_ASKED), 1);
    CHECK_EQ(s->ctx.noutbox, 0);
}

static void deliver_parts(scen *s, int i, const uint8_t transfer[16], const uint8_t *bytes,
                          size_t len, int skip_last)
{
    uint8_t count = (uint8_t)((len + LZ_MAX_PART_LEN - 1) / LZ_MAX_PART_LEN), p;
    for (p = 0; p < count - (skip_last ? 1 : 0); p++) {
        lz_message m = msg(LZ_T_HOME_CONTACT_PART);
        size_t off = (size_t)p * LZ_MAX_PART_LEN;
        memcpy(m.u.part.contact, s->c[i].own_mailbox, 32);
        memcpy(m.u.part.transfer, transfer, 16);
        m.u.part.index = p;
        m.u.part.count = count;
        m.u.part.len = (uint8_t)(len - off < LZ_MAX_PART_LEN ? len - off : LZ_MAX_PART_LEN);
        memcpy(m.u.part.data, bytes + off, m.u.part.len);
        phone_sends(s, &m);
    }
}

static void a_contact_paired_later_reaches_the_satellite(void)
{
    scen *s = &S;
    lz_contact_record r;
    uint8_t bytes[LZ_RECORD_MAX_LEN], transfer[16];
    size_t len;
    lz_message m;
    scen_init(s, 1, NULL);
    scen_keys(s, 1, "Carl");
    s->n = 2;
    fr_add(&s->fr, RELAY, s->c[1].own_mailbox, s->c[1].own_send, s->c[1].enrol);
    fr_add(&s->fr, OTHER, s->c[1].peer_mailbox, s->c[1].peer_send, NULL);
    /* Down messages about it before it arrives wait. */
    m = msg(LZ_T_HOME_RULES);
    memcpy(m.u.rules_set.contact, s->c[1].own_mailbox, 32);
    m.u.rules_set.rules.min_interval_minutes = 1;
    m.u.rules_set.rules.expiry_minutes = 2;
    m.u.rules_set.rules.weekly_limit = 3;
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.nparked, 1);
    scen_record(s, 1, &r);
    r.name.len = 6;
    memcpy(r.name.b, "Carl 1", 6);
    len = lz_contact_record_encode(&r, bytes);
    CHECK(len > LZ_MAX_PART_LEN);
    memset(transfer, 0x44, 16);
    /* Parts out of order and repeated are fine. */
    {
        lz_message p = msg(LZ_T_HOME_CONTACT_PART);
        memcpy(p.u.part.contact, s->c[1].own_mailbox, 32);
        memcpy(p.u.part.transfer, transfer, 16);
        p.u.part.index = 1;
        p.u.part.count = (uint8_t)((len + LZ_MAX_PART_LEN - 1) / LZ_MAX_PART_LEN);
        p.u.part.len = (uint8_t)(len - LZ_MAX_PART_LEN < LZ_MAX_PART_LEN ? len - LZ_MAX_PART_LEN : LZ_MAX_PART_LEN);
        memcpy(p.u.part.data, bytes + LZ_MAX_PART_LEN, p.u.part.len);
        phone_sends(s, &p);
        memset(p.id, 0, 16);
        phone_sends(s, &p);
    }
    fr_clear_events(&s->fr);
    deliver_parts(s, 1, transfer, bytes, len, 0);
    CHECK_EQ(s->ctx.ncontacts, 2);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_CONTACT_ADDED), 1);
    CHECK_EQ(s->ctx.contacts[1].own_rules.weekly_limit, 3);
    CHECK_EQ(s->ctx.nparked, 0);
    scen_advance(s, 1);
    CHECK(s->ctx.contacts[1].has_own_token);
    CHECK(fr_find(&s->fr, s->c[1].own_mailbox)->subscribed);
    /* It is read: a request shows. */
    request_from(s, 1);
    CHECK(is_open(s, 1));

    /* A record again renames it. */
    r.name.len = 4;
    memcpy(r.name.b, "Carl", 4);
    len = lz_contact_record_encode(&r, bytes);
    memset(transfer, 0x45, 16);
    fr_clear_events(&s->fr);
    deliver_parts(s, 1, transfer, bytes, len, 0);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_CONTACT_UPDATED), 1);
    CHECK_EQ(s->ctx.contacts[1].name.len, 4);
    CHECK_EQ(s->ctx.contacts[1].own_rules.weekly_limit, 3); /* older stamp: kept */

    /* A part that contradicts an earlier one discards the transfer. */
    memset(transfer, 0x46, 16);
    r.name.len = 3;
    memcpy(r.name.b, "Bad", 3);
    len = lz_contact_record_encode(&r, bytes);
    deliver_parts(s, 1, transfer, bytes, len, 1);
    {
        lz_message p = msg(LZ_T_HOME_CONTACT_PART);
        memcpy(p.u.part.contact, s->c[1].own_mailbox, 32);
        memcpy(p.u.part.transfer, transfer, 16);
        p.u.part.index = 0;
        p.u.part.count = 2;
        p.u.part.len = 5;
        phone_sends(s, &p);
    }
    deliver_parts(s, 1, transfer, bytes, len, 0);
    CHECK_EQ(s->ctx.contacts[1].name.len, 4);
    /* A count of 0 is discarded too. */
    {
        lz_message p = msg(LZ_T_HOME_CONTACT_PART);
        memcpy(p.u.part.contact, s->c[1].own_mailbox, 32);
        memset(p.u.part.transfer, 0x47, 16);
        phone_sends(s, &p);
    }
    CHECK_EQ(s->ctx.contacts[1].name.len, 4);
    /* Transfers that never complete are forgotten after 24 hours. */
    scen_advance(s, 24 * 3600 + 1);
    {
        int i, used = 0;
        for (i = 0; i < LZ_MAX_TRANSFERS; i++) used += s->ctx.transfers[i].used;
        CHECK_EQ(used, 0);
    }
}

static void the_satellite_drops_up_types_and_replays(void)
{
    scen *s = &S;
    lz_message m, r;
    uint8_t env[LZ_ENVELOPE_LEN], token[32];
    int from;
    scen_init(s, 1, NULL);
    /* An up type on the channel is dropped, before it is remembered. */
    m = msg(LZ_T_SAT_ANSWERED);
    memcpy(m.u.responded.contact, s->c[0].own_mailbox, 32);
    m.u.responded.kind = LZ_KIND_MANUAL;
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.channel.seen_n, 0);
    /* Unknown types in the down range are dropped. */
    {
        uint8_t plain[LZ_PLAINTEXT_LEN], nonce[24];
        memset(plain, 0, sizeof plain);
        plain[0] = 97;
        plain[17 + 4] = 1;
        lz_put_u64_test(plain + 17, s->w.now);
        t_rng_fill(&s->rng, plain + 1, 16);
        t_rng_fill(&s->rng, nonce, 24);
        lz_seal_plain(env, plain, s->key, nonce);
        fr_put(&s->fr, s->mailbox, env);
        fr_run(&s->fr);
        CHECK_EQ(s->ctx.channel.seen_n, 0);
    }
    /* Garbage and other keys are dropped. */
    memset(env, 0x5A, sizeof env);
    env[0] = 1;
    fr_put(&s->fr, s->mailbox, env);
    fr_run(&s->fr);
    /* A down message replayed after a restart is dropped. */
    m = msg(LZ_T_HOME_SEND_TOKEN);
    memset(m.u.send_token, 0xB1, 32);
    phone_seal(s, &m, env);
    fr_put(&s->fr, s->mailbox, env);
    fr_run(&s->fr);
    m = msg(LZ_T_HOME_SEND_TOKEN);
    memset(m.u.send_token, 0xB2, 32);
    phone_sends(s, &m);
    scen_restart(s);
    fr_put(&s->fr, s->mailbox, env);
    fr_run(&s->fr);
    memset(token, 0xB2, 32);
    CHECK_MEM(s->ctx.channel.send_token, token, 32);
    /* Messages outside the window are dropped. */
    m = msg(LZ_T_HOME_SEND_TOKEN);
    m.sent_at = s->w.now - LZ_MAX_MESSAGE_AGE - 1;
    memset(m.u.send_token, 0xB3, 32);
    phone_sends(s, &m);
    m = msg(LZ_T_HOME_SEND_TOKEN);
    m.sent_at = s->w.now + LZ_MAX_CLOCK_AHEAD + 1;
    memset(m.u.send_token, 0xB4, 32);
    phone_sends(s, &m);
    CHECK_MEM(s->ctx.channel.send_token, token, 32);

    /* A request answered, then replayed after a restart, stays answered. */
    r = msg(LZ_T_REQUEST);
    seal_from_contact(s, 0, &r, env);
    fr_put(&s->fr, s->c[0].own_mailbox, env);
    fr_run(&s->fr);
    CHECK(is_open(s, 0));
    from = s->fr.nsent;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK(!is_open(s, 0));
    scen_restart(s);
    fr_put(&s->fr, s->c[0].own_mailbox, env);
    fr_run(&s->fr);
    CHECK(!is_open(s, 0));
    /* The satellite's own response, reflected into its mailbox, is
     * dropped by the direction marker. */
    {
        int k;
        for (k = from; k < s->fr.nsent; k++) {
            if (memcmp(s->fr.sent[k].mailbox, s->c[0].peer_mailbox, 32) == 0) {
                int before = s->ctx.contacts[0].seen_n;
                fr_put(&s->fr, s->c[0].own_mailbox, s->fr.sent[k].env);
                fr_run(&s->fr);
                CHECK_EQ(s->ctx.contacts[0].seen_n, before);
            }
        }
    }
}

static void removing_the_satellite_wipes_it(void)
{
    scen *s = &S;
    uint8_t copy[200000];
    size_t copy_len;
    lz_platform pf;
    scen_init(s, 2, NULL);
    request_from(s, 0);
    copy_len = s->fr.blob_len;
    memcpy(copy, s->fr.blob, copy_len);
    /* A contact mailbox gone: only the contact goes. */
    fr_clear_events(&s->fr);
    fr_delete(&s->fr, s->c[1].own_mailbox);
    fr_run(&s->fr);
    CHECK_EQ(s->ctx.ncontacts, 1);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_CONTACT_REMOVED), 1);
    CHECK(s->ctx.installed);
    /* A reader gone on a contact mailbox decides nothing. */
    fr_revoke(&s->fr, s->c[0].own_mailbox);
    fr_run(&s->fr);
    CHECK(s->ctx.installed);
    /* The channel no longer knows the reader: everything is erased. */
    fr_clear_events(&s->fr);
    fr_revoke(&s->fr, s->mailbox);
    fr_run(&s->fr);
    CHECK(!s->ctx.installed);
    CHECK(s->ctx.removed);
    CHECK_EQ(s->fr.erases, 1);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_REMOVED), 1);
    CHECK_EQ(s->ctx.ncontacts, 0);
    CHECK(lz_is_zero_test(s->ctx.channel.key) && lz_is_zero_test(s->ctx.reader));
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
    CHECK(lz_press(&s->ctx) == LZ_E_STATE);
    /* A copy of the flash cannot read either: it wipes itself too. */
    t_platform(&pf, &s->w);
    CHECK(lz_init(&s->ctx, &pf) == LZ_OK);
    CHECK(lz_load(&s->ctx, copy, copy_len) == LZ_OK);
    s->fr.erases = 0;
    scen_connect(s);
    CHECK(!s->ctx.installed);
    CHECK_EQ(s->fr.erases, 1);
    /* The channel deleted (unknown_mailbox) also wipes. */
    scen_init(s, 1, NULL);
    fr_delete(&s->fr, s->mailbox);
    fr_run(&s->fr);
    CHECK(!s->ctx.installed);
    /* A factory reset erases, but is not a removal. */
    scen_init(s, 1, NULL);
    lz_factory_reset(&s->ctx);
    fr_run(&s->fr);
    CHECK(!s->ctx.installed && !s->ctx.removed);
}

static void an_end_from_the_contact(void)
{
    scen *s = &S;
    lz_message m;
    scen_init(s, 1, NULL);
    set_flags(s, 0, 0, 1);
    request_from(s, 0);
    fr_clear_events(&s->fr);
    m = msg(LZ_T_END);
    contact_sends(s, 0, &m);
    CHECK(s->ctx.contacts[0].ended);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_CONTACT_ENDED), 1);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_REQUEST_CLOSED), 1);
    CHECK_EQ(s->fr.led, LZ_LED_OFF);
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_ASK_REFUSED), 1);
    /* After a reconnect its mailbox is no longer read. */
    scen_restart(s);
    CHECK(!fr_find(&s->fr, s->c[0].own_mailbox)->subscribed);
}

static void a_press_offline_goes_out_later(void)
{
    scen *s = &S;
    lz_message r, got;
    uint64_t pressed;
    int f;
    scen_init(s, 1, NULL);
    r = request_from(s, 0);
    s->fr.online = 0;
    lz_online(&s->ctx, 0);
    fr_run(&s->fr);
    scen_advance(s, 60);
    pressed = s->w.now;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(s->ctx.noutbox, 2);
    f = s->fr.nsent;
    scen_advance(s, 3600);
    scen_restart(s); /* and it survives a restart */
    s->fr.online = 1;
    scen_connect(s);
    CHECK_EQ(peer_got(s, 0, &f, &got), 1);
    CHECK_EQ(got.sent_at, pressed);
    CHECK_MEM(got.u.response.request, r.id, 16);
    CHECK_EQ(s->ctx.noutbox, 0);
}

static void a_clock_that_is_off_sends_nothing(void)
{
    scen *s = &S;
    lz_message got;
    int f;
    scen_init(s, 1, NULL);
    request_from(s, 0);
    /* The relay's clock is 15 minutes ahead. */
    s->fr.date_offset = 15 * 60;
    fr_clear_events(&s->fr);
    scen_restart(s);
    CHECK(s->ctx.clock_bad);
    CHECK(fr_events(&s->fr, LZ_EV_CLOCK_WARNING) >= 1);
    CHECK_EQ(s->fr.led, LZ_LED_WARNING);
    f = s->fr.nsent;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(peer_got(s, 0, &f, &got), 0);
    CHECK_EQ(s->ctx.noutbox, 0);
    /* Back in step: it works again. */
    s->fr.date_offset = 10;
    scen_advance(s, 61);
    CHECK(!s->ctx.clock_bad);
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(peer_got(s, 0, &f, &got), 1);
    /* An unset clock does nothing at all. */
    s->w.now = 0;
    lz_press(&s->ctx);
    fr_run(&s->fr);
    CHECK_EQ(peer_got(s, 0, &f, &got), 0);
}

static void the_outbox_is_bounded_and_expires(void)
{
    scen *s = &S;
    int i;
    scen_init(s, 1, NULL);
    set_flags(s, 0, 0, 1);
    s->fr.online = 0;
    lz_online(&s->ctx, 0);
    /* Fill it directly (asking is limited by the rules); offline, nothing
     * is delivered. */
    for (i = 0; i < LZ_OUTBOX_SIZE - 1; i++) {
        s->ctx.outbox[i].to_channel = 1;
        s->ctx.outbox[i].created_at = s->w.now;
    }
    s->ctx.noutbox = LZ_OUTBOX_SIZE - 1;
    fr_clear_events(&s->fr);
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(fr_events(&s->fr, LZ_EV_OUTBOX_FULL), 1);
    CHECK_EQ(s->ctx.contacts[0].outgoing.n, 0);
    memset(s->ctx.outbox, 0, sizeof s->ctx.outbox);
    s->ctx.noutbox = 0;
    lz_ask(&s->ctx, 1);
    fr_run(&s->fr);
    CHECK_EQ(s->ctx.noutbox, 2);
    scen_advance(s, 24 * 3600);
    CHECK_EQ(s->ctx.noutbox, 0);
}

static void rules_changed_on_the_phone_reach_the_satellite(void)
{
    scen *s = &S;
    lz_message m;
    scen_init(s, 1, NULL);
    m = msg(LZ_T_HOME_RULES);
    memcpy(m.u.rules_set.contact, s->c[0].own_mailbox, 32);
    m.u.rules_set.rules.min_interval_minutes = 0;
    m.u.rules_set.rules.expiry_minutes = 60;
    m.u.rules_set.rules.weekly_limit = 1;
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.contacts[0].own_rules.weekly_limit, 1);
    /* Older rules are outdated. */
    m.sent_at = s->w.now - 10;
    memset(m.id, 0, 16);
    m.u.rules_set.rules.weekly_limit = 9;
    phone_sends(s, &m);
    CHECK_EQ(s->ctx.contacts[0].own_rules.weekly_limit, 1);
    request_from(s, 0);
    CHECK(is_open(s, 0));
    lz_press(&s->ctx);
    fr_run(&s->fr);
    scen_advance(s, 3600);
    /* Beyond the weekly limit: discarded, as on the phone. */
    request_from(s, 0);
    CHECK(!is_open(s, 0));
    /* A request that arrives after its expiry is discarded. */
    scen_advance(s, LZ_WEEK_SECS);
    {
        lz_message late = msg(LZ_T_REQUEST);
        late.sent_at = s->w.now - 61 * 60;
        contact_sends(s, 0, &late);
        CHECK(!is_open(s, 0));
    }
}

static void installing_refuses_what_does_not_fit(void)
{
    scen *s = &S;
    uint8_t setup[8192];
    size_t len;
    lz_platform pf;
    int i;
    memset(s, 0, sizeof *s);
    s->w.now = T0;
    t_rng_init(&s->w.rng, (const uint8_t *)"x", 1);
    t_platform(&pf, &s->w);
    memset(s->key, 1, 32);
    /* Four own relays (with the channel's). */
    s->n = 4;
    for (i = 0; i < 4; i++) {
        scen_keys(s, i, "X");
        snprintf(s->c[i].own_relay, sizeof s->c[i].own_relay, "https://r%d.example", i);
    }
    len = scen_setup(s, setup, sizeof setup, NULL);
    CHECK(lz_init(&s->ctx, &pf) == LZ_OK);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_E_LIMIT);
    CHECK(!s->ctx.installed);
    /* Two others are fine. */
    s->n = 2;
    len = scen_setup(s, setup, sizeof setup, NULL);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_OK);
    CHECK_EQ(s->ctx.nrelays, 3);
    /* The same contact twice. */
    memcpy(s->c[1].own_mailbox, s->c[0].own_mailbox, 32);
    len = scen_setup(s, setup, sizeof setup, NULL);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_E_VALUE);
    /* A contact with a low-order key: refused, and nothing stays. */
    scen_keys(s, 1, "Y");
    memset(s->c[1].peer_pk, 0, 32);
    len = scen_setup(s, setup, sizeof setup, NULL);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_E_CRYPTO);
    CHECK(!s->ctx.installed);
    CHECK_EQ(s->ctx.ncontacts, 0);
    /* A relay that is not https. */
    scen_keys(s, 1, "Y");
    snprintf(s->c[1].own_relay, sizeof s->c[1].own_relay, "ftp://x");
    len = scen_setup(s, setup, sizeof setup, NULL);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_E_VALUE);
    /* Cut anywhere: refused. */
    s->n = 1;
    len = scen_setup(s, setup, sizeof setup, NULL);
    for (i = 0; i < (int)len; i++) CHECK(lz_install(&s->ctx, setup, (size_t)i) != LZ_OK);
    CHECK(lz_install(&s->ctx, setup, len) == LZ_OK);
}

void suite_scenarios(void)
{
    adding_takes_own_tokens();
    a_press_answers_every_open_request();
    an_answer_elsewhere_turns_the_light_off();
    requests_made_elsewhere_reach_the_satellite();
    asking_obeys_the_rules_of_the_contact();
    reported_answers_after_the_waiting_time_only_where_allowed_once();
    a_restart_during_the_waiting_time_still_reports_once();
    states_are_shared_both_ways();
    settings_and_send_token_are_applied_in_order();
    a_contact_paired_later_reaches_the_satellite();
    the_satellite_drops_up_types_and_replays();
    removing_the_satellite_wipes_it();
    an_end_from_the_contact();
    a_press_offline_goes_out_later();
    a_clock_that_is_off_sends_nothing();
    the_outbox_is_bounded_and_expires();
    rules_changed_on_the_phone_reach_the_satellite();
    installing_refuses_what_does_not_fit();
}
