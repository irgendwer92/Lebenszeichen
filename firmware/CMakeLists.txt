# SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception
#
# Host build of lz_core and its tests (Linux, gcc and clang). SDK builds
# (ESP-IDF, Arduino, Pico SDK) compile the same sources with their own build
# files; see README.md.
cmake_minimum_required(VERSION 3.16)
project(lz_core C)

set(LZ_CRYPTO "tweetnacl" CACHE STRING "Crypto backend: tweetnacl or libsodium")
set_property(CACHE LZ_CRYPTO PROPERTY STRINGS tweetnacl libsodium)
set(LZ_SHA256 "bundled" CACHE STRING "SHA-256: bundled or mbedtls (PSA API)")
set_property(CACHE LZ_SHA256 PROPERTY STRINGS bundled mbedtls)
option(LZ_STRICT "Warnings as errors, C99, pedantic" ON)
option(LZ_SANITIZE "Build with AddressSanitizer and UndefinedBehaviorSanitizer" OFF)
option(LZ_TESTS "Build the host tests" ON)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
  set(CMAKE_BUILD_TYPE Debug)
endif()

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(LZ_SOURCES
  src/crypto.c
  src/sha256.c
  src/wire.c
  src/relay.c
  src/ledger.c
  src/satellite.c
  src/io.c
  src/store.c)

if(LZ_CRYPTO STREQUAL "tweetnacl")
  list(APPEND LZ_SOURCES third_party/tweetnacl/tweetnacl.c)
endif()

add_library(lz_core STATIC ${LZ_SOURCES})
target_include_directories(lz_core
  PUBLIC include
  PRIVATE src third_party/tweetnacl)

if(LZ_CRYPTO STREQUAL "libsodium")
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(SODIUM REQUIRED IMPORTED_TARGET libsodium)
  target_compile_definitions(lz_core PUBLIC LZ_CRYPTO_LIBSODIUM)
  target_link_libraries(lz_core PUBLIC PkgConfig::SODIUM)
elseif(NOT LZ_CRYPTO STREQUAL "tweetnacl")
  message(FATAL_ERROR "LZ_CRYPTO must be tweetnacl or libsodium")
endif()

if(LZ_SHA256 STREQUAL "mbedtls")
  find_package(MbedTLS REQUIRED)
  target_compile_definitions(lz_core PUBLIC LZ_SHA256_MBEDTLS)
  target_link_libraries(lz_core PUBLIC MbedTLS::mbedcrypto)
elseif(NOT LZ_SHA256 STREQUAL "bundled")
  message(FATAL_ERROR "LZ_SHA256 must be bundled or mbedtls")
endif()

set(LZ_WARNINGS)
if(LZ_STRICT)
  set(LZ_WARNINGS -Wall -Wextra -Werror -pedantic -Wshadow -Wstrict-prototypes
    -Wmissing-prototypes -Wvla -Wconversion -Wno-sign-conversion)
endif()
target_compile_options(lz_core PRIVATE ${LZ_WARNINGS})

if(LZ_SANITIZE)
  set(LZ_SAN_FLAGS -fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer)
  target_compile_options(lz_core PUBLIC ${LZ_SAN_FLAGS})
  target_link_options(lz_core PUBLIC ${LZ_SAN_FLAGS})
endif()

if(LZ_TESTS)
  enable_testing()
  add_subdirectory(tests)
endif()
