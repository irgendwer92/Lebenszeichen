/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* Layouts, round trips and every way a plaintext, record or setup can be
 * malformed. */
#include "test.h"

static lz_reason reason(const char *s)
{
    lz_reason r;
    r.len = (uint8_t)strlen(s);
    memcpy(r.b, s, r.len);
    return r;
}

static lz_message base(uint8_t type)
{
    lz_message m;
    memset(&m, 0, sizeof m);
    m.type = type;
    memset(m.id, 0x11, 16);
    m.sent_at = 0x0102030405060708ull;
    return m;
}

/* One message of every type the satellite knows, with a full body. */
static int sample(int i, lz_message *m)
{
    static const uint8_t types[] = {3, 4, 5, 5, 6, 80, 80, 81, 82, 83, 88, 89, 90, 91, 92, 93, 93, 94, 95, 96};
    if (i >= (int)sizeof types) return 0;
    *m = base(types[i]);
    switch (i) {
    case 0:
        m->u.rules = LZ_RULES_DEFAULT;
        break;
    case 2:
        memset(m->u.response.request, 9, 16);
        m->u.response.kind = LZ_KIND_MANUAL;
        break;
    case 3:
        memset(m->u.response.request, 9, 16);
        m->u.response.kind = LZ_KIND_REPORTED;
        m->u.response.reason = reason("schl\xc3\xa4" "ft");
        break;
    case 5:
    case 6:
        memset(m->u.responded.contact, 1, 32);
        memset(m->u.responded.request, 2, 16);
        m->u.responded.kind = i == 5 ? LZ_KIND_MANUAL : LZ_KIND_REPORTED;
        if (i == 6) m->u.responded.reason = reason("asleep");
        m->u.responded.at = 77;
        break;
    case 7:
    case 12:
        memset(m->u.requested.contact, 1, 32);
        memset(m->u.requested.request, 2, 16);
        m->u.requested.at = 66;
        break;
    case 8:
    case 17:
        m->u.state.reason = reason("Phone empty");
        m->u.state.until = 999;
        break;
    case 10:
        memset(m->u.part.contact, 1, 32);
        memset(m->u.part.transfer, 2, 16);
        m->u.part.index = 1;
        m->u.part.count = 4;
        m->u.part.len = LZ_MAX_PART_LEN;
        memset(m->u.part.data, 0x5A, LZ_MAX_PART_LEN);
        break;
    case 11:
        memset(m->u.responded.contact, 3, 32);
        memset(m->u.responded.request, 4, 16);
        m->u.responded.kind = LZ_KIND_REPORTED;
        m->u.responded.reason = reason("x");
        m->u.responded.at = 5;
        break;
    case 13:
        memset(m->u.rules_set.contact, 1, 32);
        m->u.rules_set.rules.min_interval_minutes = 1;
        m->u.rules_set.rules.expiry_minutes = 2;
        m->u.rules_set.rules.weekly_limit = 3;
        break;
    case 14:
        memset(m->u.contact_settings.contact, 1, 32);
        m->u.contact_settings.flags.reported = 1;
        m->u.contact_settings.flags.ask_button = 2;
        break;
    case 15:
        m->u.settings.waiting_minutes = 30;
        m->u.settings.preset_minutes = 480;
        m->u.settings.share = 1;
        m->u.settings.reason = reason("asleep");
        break;
    case 16:
        m->u.settings = LZ_SETTINGS_DEFAULT;
        break;
    case 19:
        memset(m->u.send_token, 0x55, 32);
        break;
    default:
        break;
    }
    return 1;
}

static void header_and_layouts(void)
{
    lz_message m = base(LZ_T_REQUEST), back;
    uint8_t p[LZ_PLAINTEXT_LEN];
    static const uint8_t rules[10] = {0, 0, 1, 104, 0, 0, 5, 160, 0, 7};
    size_t i;
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    CHECK_EQ(p[0], LZ_T_REQUEST);
    CHECK_MEM(p + 1, m.id, 16);
    CHECK_EQ(p[17], 1);
    CHECK_EQ(p[24], 8);
    CHECK_EQ(p[25], 0);
    CHECK_EQ(p[26], 0);
    for (i = 27; i < sizeof p; i++) CHECK_EQ(p[i], 0);

    m = base(LZ_T_RULES);
    m.u.rules = LZ_RULES_DEFAULT;
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    CHECK_EQ(p[26], 10);
    CHECK_MEM(p + 27, rules, 10);

    m = base(LZ_T_RESPONSE);
    memset(m.u.response.request, 9, 16);
    m.u.response.kind = LZ_KIND_REPORTED;
    m.u.response.reason = reason("schl\xc3\xa4" "ft");
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    CHECK_EQ(p[26], 26);
    CHECK_EQ(p[43], 2);
    CHECK_EQ(p[44], 8);

    /* Settings without preset are six zero bytes after the waiting time. */
    m = base(LZ_T_HOME_SETTINGS);
    m.u.settings = LZ_SETTINGS_DEFAULT;
    m.u.settings.share = 1; /* ignored without a preset */
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    CHECK_EQ(p[26], 8);
    CHECK_EQ(p[28], 15);
    for (i = 29; i < 35; i++) CHECK_EQ(p[i], 0);
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_OK);
    CHECK_EQ(back.u.settings.preset_minutes, 0);
    CHECK_EQ(back.u.settings.share, 0);

    /* A manual response's reason is read and dropped; one that is not
     * UTF-8 makes the message unreadable. */
    m = base(LZ_T_RESPONSE);
    m.u.response.kind = LZ_KIND_MANUAL;
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    p[26] = 18 + 3;
    p[44] = 3;
    memcpy(p + 45, "abc", 3);
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_OK);
    CHECK_EQ(back.u.response.reason.len, 0);
    p[45] = 0xFF;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_UTF8);

    /* Unknown and unparsed types decode with the header only. */
    m = base(LZ_T_REQUEST);
    CHECK(lz_message_encode(&m, p) == LZ_OK);
    p[0] = 1; /* hello: not parsed by the satellite */
    p[26] = 5;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_OK);
    CHECK_EQ(back.type, 1);
    p[0] = 97;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_OK);
    CHECK(!lz_type_known(97));
    CHECK(!lz_type_is_down(97));
    back.type = 97;
    CHECK(lz_message_encode(&back, p) == LZ_E_VALUE);
}

static void round_trips_and_truncation(void)
{
    lz_message m, back;
    uint8_t p[LZ_PLAINTEXT_LEN], again[LZ_PLAINTEXT_LEN];
    int i;
    for (i = 0; sample(i, &m); i++) {
        size_t cut, body;
        CHECK(lz_message_encode(&m, p) == LZ_OK);
        CHECK(lz_message_decode(&back, p, sizeof p) == LZ_OK);
        CHECK(lz_message_encode(&back, again) == LZ_OK);
        CHECK_MEM(again, p, sizeof p);
        body = ((size_t)p[25] << 8) | p[26];
        /* The plaintext cut anywhere before the body ends. */
        for (cut = 0; cut < LZ_HEADER_LEN + body; cut++) {
            CHECK(lz_message_decode(&back, p, cut) == LZ_E_TRUNCATED);
        }
        /* The body length shortened: every field is required. */
        if (body > 0) {
            size_t n;
            for (n = 0; n < body; n++) {
                uint8_t q[LZ_PLAINTEXT_LEN];
                memcpy(q, p, sizeof q);
                q[25] = (uint8_t)(n >> 8);
                q[26] = (uint8_t)n;
                CHECK(lz_message_decode(&back, q, sizeof q) != LZ_OK);
            }
        }
        /* Longer than described: the rest is ignored. */
        if (LZ_HEADER_LEN + body + 3 <= LZ_PLAINTEXT_LEN) {
            uint8_t q[LZ_PLAINTEXT_LEN];
            memcpy(q, p, sizeof q);
            q[26] = (uint8_t)(body + 3);
            q[25] = (uint8_t)((body + 3) >> 8);
            CHECK(lz_message_decode(&back, q, sizeof q) == LZ_OK);
        }
        /* A body length beyond the plaintext. */
        p[25] = 0x01;
        p[26] = 0x00;
        CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_TRUNCATED);
        p[25] = 0xFF;
        p[26] = 0xFF;
        CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_TRUNCATED);
    }
}

static void undefined_values(void)
{
    lz_message m, back;
    uint8_t p[LZ_PLAINTEXT_LEN];
    /* A response kind other than 1 and 2. */
    sample(2, &m);
    lz_message_encode(&m, p);
    p[43] = 3;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_VALUE);
    p[43] = 0;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_VALUE);
    /* A reason above 64 bytes. */
    sample(3, &m);
    lz_message_encode(&m, p);
    p[44] = 65;
    p[26] = (uint8_t)(18 + 65);
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_TOO_LONG);
    /* Reported answers allowed: only 0 and 1. */
    sample(14, &m);
    lz_message_encode(&m, p);
    p[27 + 32] = 2;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_VALUE);
    /* Share in the settings, also without a preset. */
    sample(16, &m);
    lz_message_encode(&m, p);
    p[27 + 6] = 2;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_VALUE);
    /* A part longer than 178 bytes. */
    sample(10, &m);
    lz_message_encode(&m, p);
    p[27 + 50] = 179;
    CHECK(lz_message_decode(&back, p, sizeof p) != LZ_OK);
    m.u.part.len = 179;
    CHECK(lz_message_encode(&m, p) == LZ_E_TOO_LONG);
    /* A state reason that is not UTF-8. */
    sample(8, &m);
    lz_message_encode(&m, p);
    p[28] = 0xC0;
    p[29] = 0x80;
    CHECK(lz_message_decode(&back, p, sizeof p) == LZ_E_UTF8);
}

static void utf8(void)
{
    CHECK(lz_utf8_valid((const uint8_t *)"", 0));
    CHECK(lz_utf8_valid((const uint8_t *)"schl\xc3\xa4" "ft", 8));
    CHECK(lz_utf8_valid((const uint8_t *)"\xe2\x82\xac", 3));
    CHECK(lz_utf8_valid((const uint8_t *)"\xf0\x9f\x98\x80", 4));
    CHECK(lz_utf8_valid((const uint8_t *)"\xf4\x8f\xbf\xbf", 4));
    CHECK(!lz_utf8_valid((const uint8_t *)"\xc0\x80", 2));         /* overlong */
    CHECK(!lz_utf8_valid((const uint8_t *)"\xe0\x80\xaf", 3));     /* overlong */
    CHECK(!lz_utf8_valid((const uint8_t *)"\xed\xa0\x80", 3));     /* surrogate */
    CHECK(!lz_utf8_valid((const uint8_t *)"\xf4\x90\x80\x80", 4)); /* > U+10FFFF */
    CHECK(!lz_utf8_valid((const uint8_t *)"\xe2\x82", 2));         /* cut */
    CHECK(!lz_utf8_valid((const uint8_t *)"\x80", 1));
    CHECK(!lz_utf8_valid((const uint8_t *)"\xff", 1));
    CHECK(!lz_utf8_valid((const uint8_t *)"a\xc3", 2));
}

static void envelopes(void)
{
    uint8_t a_sk[32], b_sk[32], a_pk[32], b_pk[32], k[32], nonce[24], env[LZ_ENVELOPE_LEN];
    uint8_t plain[LZ_PLAINTEXT_LEN];
    lz_message m, back;
    memset(a_sk, 1, 32);
    memset(b_sk, 2, 32);
    memset(nonce, 3, 24);
    lz_scalarmult_base(a_pk, a_sk);
    lz_scalarmult_base(b_pk, b_sk);
    CHECK(lz_box_beforenm(k, b_pk, a_sk) == 0);
    sample(3, &m);
    CHECK(lz_seal_a(env, &m, k, a_pk, b_pk, nonce) == LZ_OK);
    CHECK_EQ(env[0], 1);
    CHECK_MEM(env + 1, nonce, 24);
    /* B opens it; A, which would get it reflected, does not. */
    CHECK(lz_open_a(&back, env, sizeof env, k, b_pk, a_pk) == LZ_OK);
    CHECK_EQ(back.type, LZ_T_RESPONSE);
    CHECK(lz_open_a(&back, env, sizeof env, k, a_pk, b_pk) == LZ_E_DECRYPT);
    /* Marker 0 (older senders) is accepted, others refused. */
    CHECK(lz_open_plain(plain, env, sizeof env, k) == LZ_OK);
    CHECK_EQ(plain[255], memcmp(a_pk, b_pk, 32) < 0 ? 1 : 2);
    plain[255] = 0;
    CHECK(lz_seal_plain(env, plain, k, nonce) == LZ_OK);
    CHECK(lz_open_a(&back, env, sizeof env, k, b_pk, a_pk) == LZ_OK);
    CHECK(lz_open_a(&back, env, sizeof env, k, a_pk, b_pk) == LZ_OK);
    plain[255] = 3;
    CHECK(lz_seal_plain(env, plain, k, nonce) == LZ_OK);
    CHECK(lz_open_a(&back, env, sizeof env, k, b_pk, a_pk) == LZ_E_DECRYPT);
    CHECK(lz_open_a(&back, env, sizeof env, k, a_pk, b_pk) == LZ_E_DECRYPT);
    /* A body that reaches the marker does not fit form A. */
    sample(10, &m);
    CHECK(lz_seal_a(env, &m, k, a_pk, b_pk, nonce) == LZ_E_TOO_LONG);
    CHECK(lz_seal_c(env, &m, k, nonce) == LZ_OK);
    CHECK(lz_open_c(&back, env, sizeof env, k) == LZ_OK);
    CHECK_EQ(back.u.part.len, LZ_MAX_PART_LEN);
    /* Length and version of the envelope. */
    CHECK(lz_open_c(&back, env, sizeof env - 1, k) == LZ_E_TRUNCATED);
    CHECK(lz_open_c(&back, env, 0, k) == LZ_E_TRUNCATED);
    {
        uint8_t longer[LZ_ENVELOPE_LEN + 1];
        memcpy(longer, env, sizeof env);
        longer[LZ_ENVELOPE_LEN] = 0;
        CHECK(lz_open_c(&back, longer, sizeof longer, k) == LZ_E_TOO_LONG);
    }
    env[0] = 2;
    CHECK(lz_open_c(&back, env, sizeof env, k) == LZ_E_VERSION);
    env[0] = 1;
    env[100] ^= 1;
    CHECK(lz_open_c(&back, env, sizeof env, k) == LZ_E_DECRYPT);
}

static lz_contact_record record(void)
{
    lz_contact_record r;
    memset(&r, 0, sizeof r);
    memset(r.secret_key, 1, 32);
    memset(r.peer_key, 2, 32);
    memset(r.enrolment, 3, 32);
    r.own_relay.len = 17;
    memcpy(r.own_relay.b, "https://r.example", 17);
    memset(r.peer_mailbox, 4, 32);
    memset(r.peer_send_token, 5, 32);
    r.peer_relay = r.own_relay;
    r.own_rules = LZ_RULES_DEFAULT;
    r.own_rules_stamp.at = 0x0102030405060708ull;
    memset(r.own_rules_stamp.id, 6, 16);
    r.peer_rules.min_interval_minutes = 1;
    r.peer_rules.expiry_minutes = 2;
    r.peer_rules.weekly_limit = 3;
    r.flags.reported = 1;
    r.flags.ask_button = 2;
    memset(r.open_id, 7, 16);
    r.open_at = 9;
    r.last_answer = 8;
    r.recent_len = 2;
    r.recent[0] = 5;
    r.recent[1] = 9;
    r.name.len = 4;
    memcpy(r.name.b, "Anna", 4);
    return r;
}

static void contact_record(void)
{
    lz_contact_record r = record(), back;
    uint8_t buf[LZ_RECORD_MAX_LEN];
    size_t len = lz_contact_record_encode(&r, buf), i;
    static const uint8_t rules[10] = {0, 0, 1, 104, 0, 0, 5, 160, 0, 7};
    /* As in crates/wire/src/satellite.rs, contact_record_layout_... */
    CHECK_EQ(len, 96 + 18 + 64 + 18 + 68 + 2 + 24 + 8 + 1 + 16 + 5);
    CHECK_EQ(buf[96], 17);
    CHECK_MEM(buf + 196, rules, 10);
    CHECK_EQ(buf[264], 1);
    CHECK_EQ(buf[265], 2);
    CHECK_EQ(buf[289], 9);
    CHECK_EQ(buf[297], 8);
    CHECK_EQ(buf[298], 2);
    CHECK_EQ(buf[315], 4);
    CHECK(lz_contact_record_decode(&back, buf, len) == LZ_OK);
    CHECK_EQ(back.recent[1], 9);
    for (i = 0; i < len; i++) CHECK(lz_contact_record_decode(&back, buf, i) == LZ_E_TRUNCATED);
    buf[264] = 2;
    CHECK(lz_contact_record_decode(&back, buf, len) == LZ_E_VALUE);
    buf[264] = 1;
    buf[298] = LZ_RECENT_MAX + 1;
    CHECK(lz_contact_record_decode(&back, buf, sizeof buf) == LZ_E_TOO_LONG);
    buf[298] = 2;
    buf[316] = 0xFF; /* name not UTF-8 */
    CHECK(lz_contact_record_decode(&back, buf, len) == LZ_E_UTF8);

    /* The longest: 698 bytes. */
    r.own_relay.len = r.peer_relay.len = LZ_MAX_URL_LEN;
    memset(r.own_relay.b, 'r', LZ_MAX_URL_LEN);
    memset(r.peer_relay.b, 'r', LZ_MAX_URL_LEN);
    r.name.len = LZ_MAX_NAME_LEN;
    memset(r.name.b, 'n', LZ_MAX_NAME_LEN);
    r.recent_len = LZ_RECENT_MAX;
    CHECK_EQ(lz_contact_record_encode(&r, buf), LZ_RECORD_MAX_LEN);
    CHECK(lz_contact_record_decode(&back, buf, LZ_RECORD_MAX_LEN) == LZ_OK);
    /* Without an open request: 24 zero bytes. */
    r = record();
    memset(r.open_id, 0, 16);
    len = lz_contact_record_encode(&r, buf);
    for (i = 266; i < 290; i++) CHECK_EQ(buf[i], 0);
}

static void setup(void)
{
    lz_setup_header h, back;
    lz_contact_record r = record(), rb;
    uint8_t buf[2048], mailbox[32];
    size_t len, i, off;
    memset(&h, 0, sizeof h);
    memset(h.reader, 9, 16);
    h.wifi_name_len = 4;
    memcpy(h.wifi_name, "home", 4);
    h.wifi_password_len = 6;
    memcpy(h.wifi_password, "secret", 6);
    h.relay = r.own_relay;
    memset(h.mailbox, 1, 32);
    memset(h.key, 4, 32);
    h.settings.waiting_minutes = 30;
    h.settings.preset_minutes = 480;
    h.settings.share = 1;
    h.settings.reason = reason("asleep");
    h.contacts = 2;
    len = t_setup_header(buf, sizeof buf, &h);
    len += t_setup_entry(buf + len, sizeof buf - len, mailbox, &r);
    memset(mailbox, 0x22, 32);
    len += t_setup_entry(buf + len, sizeof buf - len, mailbox, &r);
    CHECK(lz_setup_header_decode(&back, buf, len) == LZ_OK);
    CHECK_EQ(back.contacts, 2);
    CHECK_EQ(back.settings.preset_minutes, 480);
    off = back.entries_offset;
    CHECK(lz_setup_entry_next(buf, len, &off, mailbox, &rb) == LZ_OK);
    CHECK(lz_setup_entry_next(buf, len, &off, mailbox, &rb) == LZ_OK);
    CHECK_EQ(off, len);
    CHECK(lz_setup_entry_next(buf, len, &off, mailbox, &rb) == LZ_E_TRUNCATED);
    for (i = 0; i < back.entries_offset; i++) {
        CHECK(lz_setup_header_decode(&back, buf, i) != LZ_OK);
    }
    for (i = back.entries_offset; i < len; i++) {
        size_t o = back.entries_offset;
        int e = lz_setup_entry_next(buf, i, &o, mailbox, &rb);
        if (e == LZ_OK) e = lz_setup_entry_next(buf, i, &o, mailbox, &rb);
        CHECK(e != LZ_OK);
    }
    buf[0] = 2;
    CHECK(lz_setup_header_decode(&back, buf, len) == LZ_E_VERSION);
    buf[0] = 1;
    buf[17] = 33; /* WiFi name above 32 bytes */
    CHECK(lz_setup_header_decode(&back, buf, len) == LZ_E_TOO_LONG);
}

void suite_wire(void)
{
    header_and_layouts();
    round_trips_and_truncation();
    undefined_values();
    utf8();
    envelopes();
    contact_record();
    setup();
}
