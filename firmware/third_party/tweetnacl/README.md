# TweetNaCl (bundled)

`tweetnacl.c` holds the parts of TweetNaCl (version 20140427, by Daniel J.
Bernstein, Wesley Janssen, Tanja Lange and Peter Schwabe) that `lz_core`
uses: Salsa20, HSalsa20, Poly1305, `crypto_secretbox`, X25519 and
`crypto_box_beforenm`.

**License: public domain.** TweetNaCl's authors placed it in the public
domain (<https://tweetnacl.cr.yp.to/>, "TweetNaCl is a self-contained
public-domain C library"). The rest of `firmware/` is under
LGPL-3.0-or-later with the static linking exception; this directory is
not.

The changes from the published file are listed at the top of
`tweetnacl.c`; none changes a result. The host tests compare every
function with libsodium (when it is installed) and reproduce the Rust test
vectors byte for byte.

Before a release, compare `tweetnacl.c` with the published
`tweetnacl.c` of 20140427 function by function: this copy was written
down without network access, from the published code, and is checked by
the tests rather than by a byte comparison with the original.
