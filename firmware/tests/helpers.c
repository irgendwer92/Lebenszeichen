/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <stdlib.h>
#include <string.h>

#include "test.h"

int t_failures;
int t_checks;

static int nibble(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

int t_unhex(uint8_t *out, size_t cap, const char *hex, size_t len)
{
    size_t i;
    if (len % 2 || len / 2 > cap) return -1;
    for (i = 0; i < len / 2; i++) {
        int a = nibble(hex[2 * i]), b = nibble(hex[2 * i + 1]);
        if (a < 0 || b < 0) return -1;
        out[i] = (uint8_t)(a * 16 + b);
    }
    return (int)(len / 2);
}

void t_rng_init(t_rng *r, const uint8_t *seed, size_t len)
{
    lz_sha256_state s;
    lz_sha256_init(&s);
    lz_sha256_update(&s, (const uint8_t *)"lebenszeichen-test-vectors", 26);
    lz_sha256_update(&s, seed, len);
    lz_sha256_final(&s, r->seed);
    r->counter = 0;
    r->left = 0;
}

void t_rng_fill(t_rng *r, uint8_t *out, size_t len)
{
    while (len--) {
        if (r->left == 0) {
            lz_sha256_state s;
            uint8_t c[8];
            int i;
            r->counter++;
            for (i = 0; i < 8; i++) c[i] = (uint8_t)(r->counter >> (56 - 8 * i));
            lz_sha256_init(&s);
            lz_sha256_update(&s, r->seed, 32);
            lz_sha256_update(&s, c, 8);
            lz_sha256_final(&s, r->buf);
            r->left = 32;
        }
        *out++ = r->buf[32 - r->left];
        r->left--;
    }
}

static uint64_t w_now(void *user) { return ((t_world *)user)->now; }

static int w_random(void *user, uint8_t *buf, size_t len)
{
    t_world *w = (t_world *)user;
    if (w->fail_random) return -1;
    t_rng_fill(&w->rng, buf, len);
    return 0;
}

void t_platform(lz_platform *pf, t_world *w)
{
    pf->user = w;
    pf->now = w_now;
    pf->random = w_random;
}

/* ---- a test-only JSON reader ---- */

enum { J_NULL, J_BOOL, J_NUM, J_STR, J_ARR, J_OBJ };

struct t_json {
    int type;
    long long num;
    const char *str;
    size_t slen;
    char *key; /* in an object */
    t_json *items;
    size_t n;
};

typedef struct {
    const char *p;
    size_t len, pos;
} jp;

static void ws(jp *j)
{
    while (j->pos < j->len && strchr(" \t\r\n", j->p[j->pos]) && j->p[j->pos]) j->pos++;
}

static int value(jp *j, t_json *out);

static int string(jp *j, const char **s, size_t *n)
{
    if (j->pos >= j->len || j->p[j->pos] != '"') return 0;
    j->pos++;
    *s = j->p + j->pos;
    while (j->pos < j->len && j->p[j->pos] != '"') {
        if (j->p[j->pos] == '\\') j->pos++;
        j->pos++;
    }
    if (j->pos >= j->len) return 0;
    *n = (size_t)(j->p + j->pos - *s);
    j->pos++;
    return 1;
}

static int value(jp *j, t_json *out)
{
    memset(out, 0, sizeof *out);
    ws(j);
    if (j->pos >= j->len) return 0;
    switch (j->p[j->pos]) {
    case '"':
        out->type = J_STR;
        return string(j, &out->str, &out->slen);
    case '{':
    case '[': {
        int obj = j->p[j->pos] == '{';
        char close = obj ? '}' : ']';
        out->type = obj ? J_OBJ : J_ARR;
        j->pos++;
        ws(j);
        if (j->pos < j->len && j->p[j->pos] == close) {
            j->pos++;
            return 1;
        }
        for (;;) {
            t_json item;
            const char *k = NULL;
            size_t kn = 0;
            if (obj) {
                ws(j);
                if (!string(j, &k, &kn)) return 0;
                ws(j);
                if (j->pos >= j->len || j->p[j->pos] != ':') return 0;
                j->pos++;
            }
            if (!value(j, &item)) return 0;
            if (obj) {
                item.key = (char *)malloc(kn + 1);
                memcpy(item.key, k, kn);
                item.key[kn] = '\0';
            }
            out->items = (t_json *)realloc(out->items, (out->n + 1) * sizeof *out->items);
            out->items[out->n++] = item;
            ws(j);
            if (j->pos < j->len && j->p[j->pos] == ',') {
                j->pos++;
                continue;
            }
            if (j->pos < j->len && j->p[j->pos] == close) {
                j->pos++;
                return 1;
            }
            return 0;
        }
    }
    case 'n':
        j->pos += 4;
        out->type = J_NULL;
        return 1;
    case 't':
        j->pos += 4;
        out->type = J_BOOL;
        out->num = 1;
        return 1;
    case 'f':
        j->pos += 5;
        out->type = J_BOOL;
        return 1;
    default: {
        char *end;
        out->type = J_NUM;
        out->num = strtoll(j->p + j->pos, &end, 10);
        if (end == j->p + j->pos) return 0;
        j->pos = (size_t)(end - j->p);
        return 1;
    }
    }
}

t_json *t_json_parse(const char *text, size_t len)
{
    jp j;
    t_json *root = (t_json *)calloc(1, sizeof *root);
    j.p = text;
    j.len = len;
    j.pos = 0;
    if (!value(&j, root)) {
        t_json_free(root);
        return NULL;
    }
    return root;
}

static void free_items(t_json *j)
{
    size_t i;
    for (i = 0; i < j->n; i++) {
        free_items(&j->items[i]);
        free(j->items[i].key);
    }
    free(j->items);
}

void t_json_free(t_json *j)
{
    if (!j) return;
    free_items(j);
    free(j);
}

const t_json *t_json_get(const t_json *o, const char *key)
{
    size_t i;
    if (!o || o->type != J_OBJ) return NULL;
    for (i = 0; i < o->n; i++) {
        if (strcmp(o->items[i].key, key) == 0) return &o->items[i];
    }
    return NULL;
}

size_t t_json_len(const t_json *a) { return a && a->type == J_ARR ? a->n : 0; }

const t_json *t_json_at(const t_json *a, size_t i) { return a && i < a->n ? &a->items[i] : NULL; }

const char *t_json_str(const t_json *v, size_t *len)
{
    if (!v || v->type != J_STR) {
        *len = 0;
        return "";
    }
    *len = v->slen;
    return v->str;
}

long long t_json_num(const t_json *v) { return v && v->type == J_NUM ? v->num : -1; }

int t_json_is_null(const t_json *v) { return v && v->type == J_NULL; }

char *t_read_file(const char *path, size_t *len)
{
    FILE *f = fopen(path, "rb");
    char *buf;
    long n;
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    n = ftell(f);
    fseek(f, 0, SEEK_SET);
    buf = (char *)malloc((size_t)n + 1);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) {
        free(buf);
        fclose(f);
        return NULL;
    }
    buf[n] = '\0';
    fclose(f);
    *len = (size_t)n;
    return buf;
}

/* ---- setup encoder ---- */

static size_t put(uint8_t *out, size_t cap, size_t pos, const void *b, size_t n)
{
    if (pos == (size_t)-1 || n > cap - pos) return (size_t)-1;
    memcpy(out + pos, b, n);
    return pos + n;
}

static size_t put_u64(uint8_t *out, size_t cap, size_t pos, uint64_t v)
{
    uint8_t b[8];
    int i;
    for (i = 0; i < 8; i++) b[i] = (uint8_t)(v >> (56 - 8 * i));
    return put(out, cap, pos, b, 8);
}

size_t t_setup_header(uint8_t *out, size_t cap, const lz_setup_header *h)
{
    size_t p = 0;
    uint8_t v = LZ_SETUP_VERSION, b[6];
    p = put(out, cap, p, &v, 1);
    p = put(out, cap, p, h->reader, 16);
    p = put(out, cap, p, &h->wifi_name_len, 1);
    p = put(out, cap, p, h->wifi_name, h->wifi_name_len);
    p = put(out, cap, p, &h->wifi_password_len, 1);
    p = put(out, cap, p, h->wifi_password, h->wifi_password_len);
    p = put(out, cap, p, &h->relay.len, 1);
    p = put(out, cap, p, h->relay.b, h->relay.len);
    p = put(out, cap, p, h->mailbox, 32);
    p = put(out, cap, p, h->enrolment, 32);
    p = put(out, cap, p, h->send_token, 32);
    p = put(out, cap, p, h->key, 32);
    p = put_u64(out, cap, p, h->settings_stamp.at);
    p = put(out, cap, p, h->settings_stamp.id, 16);
    b[0] = (uint8_t)(h->settings.waiting_minutes >> 8);
    b[1] = (uint8_t)h->settings.waiting_minutes;
    b[2] = (uint8_t)(h->settings.preset_minutes >> 24);
    b[3] = (uint8_t)(h->settings.preset_minutes >> 16);
    b[4] = (uint8_t)(h->settings.preset_minutes >> 8);
    b[5] = (uint8_t)h->settings.preset_minutes;
    p = put(out, cap, p, b, 6);
    v = h->settings.preset_minutes ? h->settings.share : 0;
    p = put(out, cap, p, &v, 1);
    v = h->settings.preset_minutes ? h->settings.reason.len : 0;
    p = put(out, cap, p, &v, 1);
    p = put(out, cap, p, h->settings.reason.b, v);
    p = put(out, cap, p, &h->contacts, 1);
    return p == (size_t)-1 ? 0 : p;
}

size_t t_setup_entry(uint8_t *out, size_t cap, const uint8_t mailbox[32],
                     const lz_contact_record *r)
{
    uint8_t rec[LZ_RECORD_MAX_LEN], len[2];
    size_t n = lz_contact_record_encode(r, rec), p = 0;
    len[0] = (uint8_t)(n >> 8);
    len[1] = (uint8_t)n;
    p = put(out, cap, p, mailbox, 32);
    p = put(out, cap, p, len, 2);
    p = put(out, cap, p, rec, n);
    return p == (size_t)-1 ? 0 : p;
}
