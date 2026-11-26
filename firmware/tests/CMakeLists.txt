# SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception
add_executable(lz_tests
  main.c
  helpers.c
  fakerelay.c
  test_crypto.c
  test_wire.c
  test_relay.c
  test_store.c
  test_vectors.c
  test_scenarios.c
  test_fuzz.c)
target_link_libraries(lz_tests PRIVATE lz_core)
target_include_directories(lz_tests PRIVATE ${CMAKE_CURRENT_SOURCE_DIR} ${PROJECT_SOURCE_DIR}/src)
if(LZ_STRICT)
  target_compile_options(lz_tests PRIVATE -Wall -Wextra -Werror -pedantic -Wvla)
endif()

# Compare TweetNaCl with libsodium where libsodium is installed.
find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
  pkg_check_modules(TEST_SODIUM QUIET IMPORTED_TARGET libsodium)
endif()
if(TEST_SODIUM_FOUND)
  target_compile_definitions(lz_tests PRIVATE LZ_TEST_SODIUM)
  target_link_libraries(lz_tests PRIVATE PkgConfig::TEST_SODIUM)
  message(STATUS "lz_tests: comparing with libsodium")
endif()

set(LZ_VECTORS ${CMAKE_CURRENT_SOURCE_DIR}/vectors/m10.json CACHE FILEPATH
  "Test vectors written by the Rust test vectors_are_reproducible")

foreach(suite crypto wire relay store scenarios fuzz)
  add_test(NAME ${suite} COMMAND lz_tests ${suite})
endforeach()
add_test(NAME vectors COMMAND lz_tests vectors ${LZ_VECTORS})
