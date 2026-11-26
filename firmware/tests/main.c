/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <stdlib.h>

#include "test.h"

int main(int argc, char **argv)
{
    const char *suite = argc > 1 ? argv[1] : "all";
    int all = strcmp(suite, "all") == 0, ran = 0;
    if (lz_crypto_init() != 0) return 2;
    if (all || strcmp(suite, "crypto") == 0) suite_crypto(), ran++;
    if (all || strcmp(suite, "wire") == 0) suite_wire(), ran++;
    if (all || strcmp(suite, "relay") == 0) suite_relay(), ran++;
    if (all || strcmp(suite, "store") == 0) suite_store(), ran++;
    if (all || strcmp(suite, "scenarios") == 0) suite_scenarios(), ran++;
    if (all || strcmp(suite, "fuzz") == 0) suite_fuzz(), ran++;
    if (strcmp(suite, "vectors") == 0 || (all && argc > 2)) {
        if (argc < 3) {
            fprintf(stderr, "usage: %s vectors <file>\n", argv[0]);
            return 2;
        }
        suite_vectors(argv[2]);
        ran++;
    }
    if (!ran) {
        fprintf(stderr, "unknown suite %s\n", suite);
        return 2;
    }
    printf("%s: %d checks, %d failed\n", suite, t_checks, t_failures);
    return t_failures ? 1 : 0;
}
