# SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception
#
# ESP-IDF component: the portable core lz_core (../../src) and its ESP-IDF
# adapter (WiFi, NTP, HTTPS, WebSocket, NVS, USB serial setup).
set(core ../../src)
idf_component_register(
  SRCS
    ${core}/crypto.c ${core}/sha256.c ${core}/wire.c ${core}/relay.c
    ${core}/ledger.c ${core}/satellite.c ${core}/io.c ${core}/store.c
    lz_esp.c lz_esp_net.c lz_esp_setup.c
  INCLUDE_DIRS include ../../include
  PRIV_INCLUDE_DIRS ${core}
  REQUIRES esp_event
  PRIV_REQUIRES esp_wifi esp_netif nvs_flash esp_http_client esp-tls mbedtls
                esp_timer esp_driver_usb_serial_jtag bootloader_support)

# libsodium for crypto_box/crypto_secretbox, mbedTLS (PSA) for SHA-256.
target_compile_definitions(${COMPONENT_LIB} PUBLIC LZ_CRYPTO_LIBSODIUM LZ_SHA256_MBEDTLS)
if(CONFIG_LZ_ALLOW_HTTP)
  target_compile_definitions(${COMPONENT_LIB} PUBLIC LZ_ALLOW_HTTP=1)
endif()
target_compile_options(${COMPONENT_LIB} PRIVATE -Wall -Wextra -Werror)
