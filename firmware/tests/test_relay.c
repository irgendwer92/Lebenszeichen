/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/* b64url, hex, the frames the satellite writes, and the frames it reads,
 * well-formed and not. */
#include "test.h"

static int parse(lz_frame *f, const char *s) { return lz_frame_parse(f, s, strlen(s)); }

static void b64url(void)
{
    uint8_t in[32], out[32];
    char s[64];
    size_t i, n;
    for (i = 0; i < 32; i++) in[i] = (uint8_t)(i * 37 + 251);
    for (i = 0; i <= 32; i++) {
        n = lz_b64url_encode(s, sizeof s, in, i);
        CHECK_EQ(n, LZ_B64URL_LEN(i));
        CHECK(lz_b64url_decode(out, i, s, n) == LZ_OK);
        CHECK_MEM(out, in, i);
        CHECK(strchr(s, '=') == NULL);
    }
    n = lz_b64url_encode(s, sizeof s, (const uint8_t *)"\xfb\xff", 2);
    CHECK(memcmp(s, "-_8", 3) == 0);
    CHECK(lz_b64url_encode(s, 2, in, 32) == 0);
    /* Wrong length, other alphabets, padding, non-zero trailing bits. */
    CHECK(lz_b64url_decode(out, 2, "-_8", 2) != LZ_OK);
    CHECK(lz_b64url_decode(out, 2, "+/8", 3) != LZ_OK);
    CHECK(lz_b64url_decode(out, 1, "AA=", 3) != LZ_OK);
    CHECK(lz_b64url_decode(out, 1, "AB", 2) != LZ_OK);
    CHECK(lz_b64url_decode(out, 1, "AA", 2) == LZ_OK);
    CHECK(lz_b64url_decode(out, 2, "AAB", 3) != LZ_OK);
    CHECK(lz_b64url_decode(out, 1, "A\0", 2) != LZ_OK);
    lz_hex_encode(s, (const uint8_t *)"\x00\xab\xff", 3);
    CHECK(memcmp(s, "00abff", 6) == 0);
}

static void writing(void)
{
    uint8_t mailbox[32], token[32], reader[16];
    char out[400], b[64], want[400];
    size_t n;
    memset(mailbox, 1, 32);
    memset(token, 2, 32);
    memset(reader, 3, 16);
    n = lz_frame_subscribe(out, sizeof out, mailbox, token, reader);
    strcpy(want, "{\"op\":\"subscribe\",\"mailbox\":\"");
    lz_b64url_encode(b, sizeof b, mailbox, 32);
    strcat(want, b);
    strcat(want, "\",\"token\":\"");
    lz_b64url_encode(b, sizeof b, token, 32);
    strcat(want, b);
    strcat(want, "\",\"reader\":\"");
    lz_b64url_encode(b, sizeof b, reader, 16);
    strcat(want, b);
    strcat(want, "\"}");
    CHECK_EQ(n, strlen(want));
    CHECK(strcmp(out, want) == 0);
    CHECK(lz_frame_subscribe(out, n, mailbox, token, reader) == 0);
    CHECK(lz_frame_subscribe(out, n + 1, mailbox, token, reader) == n);

    n = lz_frame_ack(out, sizeof out, mailbox, 18446744073709551615ull);
    lz_b64url_encode(b, sizeof b, mailbox, 32);
    snprintf(want, sizeof want, "{\"op\":\"ack\",\"mailbox\":\"%s\",\"seq\":18446744073709551615}", b);
    CHECK(strcmp(out, want) == 0);
    CHECK_EQ(n, strlen(want));
    n = lz_frame_ack(out, sizeof out, mailbox, 0);
    CHECK(strstr(out, "\"seq\":0}") != NULL);

    {
        uint8_t h[32];
        char hex[65];
        lz_sha256(h, token, 32);
        lz_hex_encode(hex, h, 32);
        hex[64] = '\0';
        n = lz_body_token_hash(out, sizeof out, token);
        snprintf(want, sizeof want, "{\"token_hash\":\"%s\"}", hex);
        CHECK(strcmp(out, want) == 0);
        CHECK_EQ(n, strlen(want));
    }
}

static void reading(void)
{
    lz_frame f;
    uint8_t mailbox[32], env[LZ_ENVELOPE_LEN];
    char mb[64], data[400], s[800];
    memset(mailbox, 7, 32);
    memset(env, 1, sizeof env);
    lz_b64url_encode(mb, sizeof mb, mailbox, 32);
    lz_b64url_encode(data, sizeof data, env, sizeof env);

    snprintf(s, sizeof s, "{\"op\":\"subscribed\",\"mailbox\":\"%s\"}", mb);
    CHECK(parse(&f, s) == LZ_OK);
    CHECK_EQ(f.op, LZ_FRAME_SUBSCRIBED);
    CHECK_MEM(f.mailbox, mailbox, 32);

    snprintf(s, sizeof s, " { \"data\" : \"%s\" , \"seq\":17,\"op\":\"message\",\"mailbox\":\"%s\","
             "\"extra\":{\"a\":[1,2.5e3,-0,true,false,null,\"x\\\"y\\u00e4\"]}} \n", data, mb);
    CHECK(parse(&f, s) == LZ_OK);
    CHECK_EQ(f.op, LZ_FRAME_MESSAGE);
    CHECK_EQ(f.seq, 17);
    CHECK_MEM(f.data, env, sizeof env);

    snprintf(s, sizeof s, "{\"op\":\"error\",\"mailbox\":\"%s\",\"code\":\"unknown_reader\"}", mb);
    CHECK(parse(&f, s) == LZ_OK);
    CHECK_EQ(f.op, LZ_FRAME_ERROR);
    CHECK(f.has_mailbox);
    CHECK(strcmp(f.code, "unknown_reader") == 0);
    CHECK(parse(&f, "{\"op\":\"error\",\"code\":\"bad_request\"}") == LZ_OK);
    CHECK(!f.has_mailbox);
    CHECK(parse(&f, "{\"op\":\"pong\"}") == LZ_OK);
    CHECK_EQ(f.op, LZ_FRAME_OTHER);
}

static void malformed(void)
{
    lz_frame f;
    char mb[64], data[400], s[5000];
    uint8_t mailbox[32], env[LZ_ENVELOPE_LEN];
    int i;
    static const char *bad[] = {
        "",
        " ",
        "[]",
        "null",
        "{",
        "{}",
        "{\"op\":\"subscribed\"}",
        "{\"op\":1}",
        "{\"op\":\"error\"}",
        "{\"op\":\"error\",\"code\":\"\"}",
        "{\"op\":\"error\",\"code\":\"UPPER\"}",
        "{\"op\":\"error\",\"code\":\"a\\nb\"}",
        "{\"op\":\"error\",\"code\":\"x\"}x",
        "{\"op\":\"error\",\"code\":\"x\",}",
        "{\"op\":\"error\",\"code\":\"x\" \"y\":1}",
        "{\"op\":\"error\",\"code\":\"x\",\"op\":\"error\"}",
        "{\"op\":\"err\\u006fr\",\"code\":\"x\"}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":01}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":1.}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":tru}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":\"\\q\"}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":\"\\u12\"}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":[[[[[[[[[[1]]]]]]]]]]}",
        "{\"op\":\"error\",\"code\":\"abcdefghijklmnopqrstuvwxyzabcdefg\"}",
        "{\"op\":\"error\",\"code\":\"x\",\"z\":\"\x01\"}",
        "{'op':'error'}",
    };
    for (i = 0; i < (int)(sizeof bad / sizeof bad[0]); i++) {
        if (parse(&f, bad[i]) == LZ_OK) {
            fprintf(stderr, "accepted: %s\n", bad[i]);
            CHECK(0);
        }
    }
    memset(mailbox, 7, 32);
    memset(env, 1, sizeof env);
    lz_b64url_encode(mb, sizeof mb, mailbox, 32);
    lz_b64url_encode(data, sizeof data, env, sizeof env);
    /* message: every field required and exact. */
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":1}", mb);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"seq\":1,\"data\":\"%s\"}", data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":1,\"data\":\"%.395s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":1,\"data\":\"%sAA\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%.42s\",\"seq\":1,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":-1,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":1.5,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":18446744073709551616,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":\"1\",\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":1,\"seq\":2,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) != LZ_OK);
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":18446744073709551615,\"data\":\"%s\"}", mb, data);
    CHECK(parse(&f, s) == LZ_OK);
    /* Above 4 KiB. */
    memset(s, ' ', sizeof s);
    s[0] = '{';
    s[4097] = '}';
    CHECK(lz_frame_parse(&f, s, 4098) != LZ_OK);
    /* Every prefix of a valid frame is refused. */
    snprintf(s, sizeof s, "{\"op\":\"message\",\"mailbox\":\"%s\",\"seq\":9,\"data\":\"%s\"}", mb, data);
    for (i = 0; i < (int)strlen(s); i++) CHECK(lz_frame_parse(&f, s, (size_t)i) != LZ_OK);
}

static void bodies_and_addresses(void)
{
    char code[LZ_MAX_ERROR_CODE + 1], out[300];
    lz_url u;
    CHECK(lz_parse_error_body(code, "{\"error\":\"unknown_mailbox\"}", strlen("{\"error\":\"unknown_mailbox\"}")) == LZ_OK);
    CHECK(strcmp(code, "unknown_mailbox") == 0);
    CHECK(lz_parse_error_body(code, "{\"x\":[1],\"error\":\"rate_limited\"}", strlen("{\"x\":[1],\"error\":\"rate_limited\"}")) == LZ_OK);
    CHECK(strcmp(code, "rate_limited") == 0);
    CHECK(lz_parse_error_body(code, "{\"error\":7}", strlen("{\"error\":7}")) != LZ_OK);
    CHECK(lz_parse_error_body(code, "<html>", strlen("<html>")) != LZ_OK);
    CHECK(code[0] == '\0');
    CHECK(lz_health_has_reader_tokens("{\"features\":[\"reader_tokens\"]}", strlen("{\"features\":[\"reader_tokens\"]}")));
    CHECK(lz_health_has_reader_tokens("{\"features\":[\"a\",1,\"reader_tokens\"],\"v\":{}}", strlen("{\"features\":[\"a\",1,\"reader_tokens\"],\"v\":{}}")));
    CHECK(!lz_health_has_reader_tokens("", strlen("")));
    CHECK(!lz_health_has_reader_tokens("{\"features\":[]}", strlen("{\"features\":[]}")));
    CHECK(!lz_health_has_reader_tokens("{\"features\":[\"reader_tokens\"]", strlen("{\"features\":[\"reader_tokens\"]")));
    CHECK(!lz_health_has_reader_tokens("{\"other\":\"reader_tokens\"}", strlen("{\"other\":\"reader_tokens\"}")));

#define URL(s) (u.len = (uint8_t)strlen(s), memcpy(u.b, s, u.len), &u)
    CHECK(lz_url_ok(URL("https://relay.example")));
    CHECK(lz_url_ok(URL("https://relay.example:8443/base")));
    CHECK(!lz_url_ok(URL("https://")));
    CHECK(!lz_url_ok(URL("https:///x")));
    CHECK(!lz_url_ok(URL("https://relay.example/")));
    CHECK(!lz_url_ok(URL("https://re lay")));
    CHECK(!lz_url_ok(URL("https://re\"lay")));
    CHECK(!lz_url_ok(URL("ftp://relay")));
    CHECK(!lz_url_ok(URL("relay.example")));
    CHECK_EQ(lz_url_ok(URL("http://localhost:8080")), LZ_ALLOW_HTTP);
    CHECK(lz_stream_url(out, sizeof out, URL("https://relay.example")) == 29);
    CHECK(strcmp(out, "wss://relay.example/v1/stream") == 0);
    CHECK(lz_stream_url(out, 10, URL("https://relay.example")) == 0);
    CHECK(lz_stream_url(out, sizeof out, URL("https://x/")) == 0);
#undef URL
}

void suite_relay(void)
{
    b64url();
    writing();
    reading();
    malformed();
    bodies_and_addresses();
}
