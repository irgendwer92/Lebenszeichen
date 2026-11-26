/*
 * TweetNaCl: a compact NaCl by Daniel J. Bernstein, Wesley Janssen,
 * Tanja Lange and Peter Schwabe (version 20140427). Public domain.
 *
 * Only the functions lz_core needs are kept (Salsa20, HSalsa20, Poly1305,
 * secretbox, X25519, box beforenm); hashing, signatures and key generation
 * are left out. Changes from the published code, none of which change a
 * result:
 *
 * - stdint types instead of `unsigned long` and `long long`;
 * - functions renamed with the prefix lz_tweetnacl_ and made static where
 *   lz_core does not call them, so that they never clash with libsodium;
 * - `sigma` written as bytes (no unterminated string initialiser);
 * - in car25519 the carry is multiplied instead of shifted left, since a
 *   left shift of a negative value is undefined in C.
 *
 * The host tests check every function against libsodium and the Rust
 * test vectors.
 */
#include "tweetnacl.h"

#define FOR(i, n) for (i = 0; i < n; ++i)
#define sv static void

typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int64_t i64;
typedef i64 gf[16];

static const u8 _0[16] = {0};
static const u8 _9[32] = {9};
static const gf _121665 = {0xDB41, 1};

static u32 L32(u32 x, int c) { return (x << c) | ((x & 0xffffffff) >> (32 - c)); }

static u32 ld32(const u8 *x)
{
    u32 u = x[3];
    u = (u << 8) | x[2];
    u = (u << 8) | x[1];
    return (u << 8) | x[0];
}

sv st32(u8 *x, u32 u)
{
    int i;
    FOR(i, 4)
    {
        x[i] = (u8)u;
        u >>= 8;
    }
}

static int vn(const u8 *x, const u8 *y, int n)
{
    u32 d = 0;
    int i;
    FOR(i, n) d |= x[i] ^ y[i];
    return (1 & ((d - 1) >> 8)) - 1;
}

static int crypto_verify_16(const u8 *x, const u8 *y) { return vn(x, y, 16); }

int lz_tweetnacl_verify_32(const u8 *x, const u8 *y) { return vn(x, y, 32); }

sv core(u8 *out, const u8 *in, const u8 *k, const u8 *c, int h)
{
    u32 w[16], x[16], y[16], t[4];
    int i, j, m;

    FOR(i, 4)
    {
        x[5 * i] = ld32(c + 4 * i);
        x[1 + i] = ld32(k + 4 * i);
        x[6 + i] = ld32(in + 4 * i);
        x[11 + i] = ld32(k + 16 + 4 * i);
    }

    FOR(i, 16) y[i] = x[i];

    FOR(i, 20)
    {
        FOR(j, 4)
        {
            FOR(m, 4) t[m] = x[(5 * j + 4 * m) % 16];
            t[1] ^= L32(t[0] + t[3], 7);
            t[2] ^= L32(t[1] + t[0], 9);
            t[3] ^= L32(t[2] + t[1], 13);
            t[0] ^= L32(t[3] + t[2], 18);
            FOR(m, 4) w[4 * j + (j + m) % 4] = t[m];
        }
        FOR(m, 16) x[m] = w[m];
    }

    if (h) {
        FOR(i, 16) x[i] += y[i];
        FOR(i, 4)
        {
            x[5 * i] -= ld32(c + 4 * i);
            x[6 + i] -= ld32(in + 4 * i);
        }
        FOR(i, 4)
        {
            st32(out + 4 * i, x[5 * i]);
            st32(out + 16 + 4 * i, x[6 + i]);
        }
    } else {
        FOR(i, 16) st32(out + 4 * i, x[i] + y[i]);
    }
}

static const u8 sigma[16] = {'e', 'x', 'p', 'a', 'n', 'd', ' ', '3',
                             '2', '-', 'b', 'y', 't', 'e', ' ', 'k'};

static int crypto_stream_salsa20_xor(u8 *c, const u8 *m, u64 b, const u8 *n, const u8 *k)
{
    u8 z[16], x[64];
    u32 u, i;
    if (!b) return 0;
    FOR(i, 16) z[i] = 0;
    FOR(i, 8) z[i] = n[i];
    while (b >= 64) {
        core(x, z, k, sigma, 0);
        FOR(i, 64) c[i] = (u8)((m ? m[i] : 0) ^ x[i]);
        u = 1;
        for (i = 8; i < 16; ++i) {
            u += (u32)z[i];
            z[i] = (u8)u;
            u >>= 8;
        }
        b -= 64;
        c += 64;
        if (m) m += 64;
    }
    if (b) {
        core(x, z, k, sigma, 0);
        FOR(i, b) c[i] = (u8)((m ? m[i] : 0) ^ x[i]);
    }
    return 0;
}

static int crypto_stream_xor(u8 *c, const u8 *m, u64 d, const u8 *n, const u8 *k)
{
    u8 s[32];
    core(s, n, k, sigma, 1);
    return crypto_stream_salsa20_xor(c, m, d, n + 16, s);
}

static int crypto_stream(u8 *c, u64 d, const u8 *n, const u8 *k)
{
    return crypto_stream_xor(c, 0, d, n, k);
}

sv add1305(u32 *h, const u32 *c)
{
    u32 j, u = 0;
    FOR(j, 17)
    {
        u += h[j] + c[j];
        h[j] = u & 255;
        u >>= 8;
    }
}

static const u32 minusp[17] = {5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 252};

static int crypto_onetimeauth(u8 *out, const u8 *m, u64 n, const u8 *k)
{
    u32 s, i, j, u, x[17], r[17], h[17], c[17], g[17];

    FOR(j, 17) r[j] = h[j] = 0;
    FOR(j, 16) r[j] = k[j];
    r[3] &= 15;
    r[4] &= 252;
    r[7] &= 15;
    r[8] &= 252;
    r[11] &= 15;
    r[12] &= 252;
    r[15] &= 15;

    while (n > 0) {
        FOR(j, 17) c[j] = 0;
        for (j = 0; (j < 16) && (j < n); ++j) c[j] = m[j];
        c[j] = 1;
        m += j;
        n -= j;
        add1305(h, c);
        FOR(i, 17)
        {
            x[i] = 0;
            FOR(j, 17) x[i] += h[j] * ((j <= i) ? r[i - j] : 320 * r[i + 17 - j]);
        }
        FOR(i, 17) h[i] = x[i];
        u = 0;
        FOR(j, 16)
        {
            u += h[j];
            h[j] = u & 255;
            u >>= 8;
        }
        u += h[16];
        h[16] = u & 3;
        u = 5 * (u >> 2);
        FOR(j, 16)
        {
            u += h[j];
            h[j] = u & 255;
            u >>= 8;
        }
        u += h[16];
        h[16] = u;
    }

    FOR(j, 17) g[j] = h[j];
    add1305(h, minusp);
    s = (u32)(-(i64)(h[16] >> 7));
    FOR(j, 17) h[j] ^= s & (g[j] ^ h[j]);

    FOR(j, 16) c[j] = k[j + 16];
    c[16] = 0;
    add1305(h, c);
    FOR(j, 16) out[j] = (u8)h[j];
    return 0;
}

static int crypto_onetimeauth_verify(const u8 *h, const u8 *m, u64 n, const u8 *k)
{
    u8 x[16];
    crypto_onetimeauth(x, m, n, k);
    return crypto_verify_16(h, x);
}

int lz_tweetnacl_secretbox(u8 *c, const u8 *m, u64 d, const u8 *n, const u8 *k)
{
    int i;
    if (d < 32) return -1;
    crypto_stream_xor(c, m, d, n, k);
    crypto_onetimeauth(c + 16, c + 32, d - 32, c);
    FOR(i, 16) c[i] = 0;
    return 0;
}

int lz_tweetnacl_secretbox_open(u8 *m, const u8 *c, u64 d, const u8 *n, const u8 *k)
{
    int i;
    u8 x[32];
    if (d < 32) return -1;
    crypto_stream(x, 32, n, k);
    if (crypto_onetimeauth_verify(c + 16, c + 32, d - 32, x) != 0) return -1;
    crypto_stream_xor(m, c, d, n, k);
    FOR(i, 32) m[i] = 0;
    return 0;
}

sv car25519(gf o)
{
    int i;
    i64 c;
    FOR(i, 16)
    {
        o[i] += ((i64)1 << 16);
        c = o[i] >> 16;
        o[(i + 1) * (i < 15)] += c - 1 + 37 * (c - 1) * (i == 15);
        o[i] -= c * ((i64)1 << 16);
    }
}

sv sel25519(gf p, gf q, int b)
{
    i64 t, c = ~((i64)b - 1);
    int i;
    FOR(i, 16)
    {
        t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

sv pack25519(u8 *o, const gf n)
{
    int i, j, b;
    gf m, t;
    FOR(i, 16) t[i] = n[i];
    car25519(t);
    car25519(t);
    car25519(t);
    FOR(j, 2)
    {
        m[0] = t[0] - 0xffed;
        for (i = 1; i < 15; i++) {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        b = (int)((m[15] >> 16) & 1);
        m[14] &= 0xffff;
        sel25519(t, m, 1 - b);
    }
    FOR(i, 16)
    {
        o[2 * i] = (u8)(t[i] & 0xff);
        o[2 * i + 1] = (u8)(t[i] >> 8);
    }
}

sv unpack25519(gf o, const u8 *n)
{
    int i;
    FOR(i, 16) o[i] = n[2 * i] + ((i64)n[2 * i + 1] << 8);
    o[15] &= 0x7fff;
}

sv A(gf o, const gf a, const gf b)
{
    int i;
    FOR(i, 16) o[i] = a[i] + b[i];
}

sv Z(gf o, const gf a, const gf b)
{
    int i;
    FOR(i, 16) o[i] = a[i] - b[i];
}

sv M(gf o, const gf a, const gf b)
{
    i64 t[31];
    int i, j;
    FOR(i, 31) t[i] = 0;
    FOR(i, 16) FOR(j, 16) t[i + j] += a[i] * b[j];
    FOR(i, 15) t[i] += 38 * t[i + 16];
    FOR(i, 16) o[i] = t[i];
    car25519(o);
    car25519(o);
}

sv S(gf o, const gf a) { M(o, a, a); }

sv inv25519(gf o, const gf i)
{
    gf c;
    int a;
    FOR(a, 16) c[a] = i[a];
    for (a = 253; a >= 0; a--) {
        S(c, c);
        if (a != 2 && a != 4) M(c, c, i);
    }
    FOR(a, 16) o[a] = c[a];
}

int lz_tweetnacl_scalarmult(u8 *q, const u8 *n, const u8 *p)
{
    u8 z[32];
    i64 x[80];
    int r, i;
    gf a, b, c, d, e, f;
    FOR(i, 31) z[i] = n[i];
    z[31] = (u8)((n[31] & 127) | 64);
    z[0] &= 248;
    unpack25519(x, p);
    FOR(i, 16)
    {
        b[i] = x[i];
        d[i] = a[i] = c[i] = 0;
    }
    a[0] = d[0] = 1;
    for (i = 254; i >= 0; --i) {
        r = (z[i >> 3] >> (i & 7)) & 1;
        sel25519(a, b, r);
        sel25519(c, d, r);
        A(e, a, c);
        Z(a, a, c);
        A(c, b, d);
        Z(b, b, d);
        S(d, e);
        S(f, a);
        M(a, c, a);
        M(c, b, e);
        A(e, a, c);
        Z(a, a, c);
        S(b, a);
        Z(c, d, f);
        M(a, c, _121665);
        A(a, a, d);
        M(c, c, a);
        M(a, d, f);
        M(d, b, x);
        S(b, e);
        sel25519(a, b, r);
        sel25519(c, d, r);
    }
    FOR(i, 16)
    {
        x[i + 16] = a[i];
        x[i + 32] = c[i];
        x[i + 48] = b[i];
        x[i + 64] = d[i];
    }
    inv25519(x + 32, x + 32);
    M(x + 16, x + 16, x + 32);
    pack25519(q, x + 16);
    FOR(i, 32) z[i] = 0;
    return 0;
}

int lz_tweetnacl_scalarmult_base(u8 *q, const u8 *n) { return lz_tweetnacl_scalarmult(q, n, _9); }

int lz_tweetnacl_box_beforenm(u8 *k, const u8 *y, const u8 *x)
{
    u8 s[32];
    int i;
    lz_tweetnacl_scalarmult(s, x, y);
    core(k, _0, s, sigma, 1);
    FOR(i, 32) s[i] = 0;
    return 0;
}
