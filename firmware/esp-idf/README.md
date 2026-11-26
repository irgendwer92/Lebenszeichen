# ESP-IDF: the adapter and the ESP32-C6 satellite

| Path | Content |
|---|---|
| `lebenszeichen/` | ESP-IDF component: `lz_core` (`../../src`) and its adapter (`lz_esp.h`): WiFi, NTP, HTTPS and WebSocket with the certificate bundle, NVS, setup over USB serial |
| `satellite-c6/` | Firmware for the ESP32-C6 SuperMini with an SSD1306 OLED, two buttons and the onboard RGB LED |

Built with ESP-IDF 6.1 (`idf: ">=6.0"`). Crypto comes from libsodium
(`espressif/libsodium`), SHA-256 from mbedTLS through PSA; the WebSocket
client is `espressif/esp_websocket_client`, the LED driver
`espressif/led_strip`.

## The board

ESP32-C6 SuperMini (4 MB flash), SSD1306 128×64 on I²C (address `0x3C`),
two push buttons to GND (no resistors: internal pull-ups).

| Part | Pin |
|---|---|
| OLED SDA | GPIO20 |
| OLED SCL | GPIO19 |
| OLED VCC / GND | 3V3 / GND |
| Answer button | GPIO18 → GND |
| Ask button | GPIO14 → GND |
| RGB LED (onboard WS2812) | GPIO8 |

GPIO4, 5, 8, 9 and 15 are strapping pins and 12/13 are USB; GPIO8 only
matters at power-on, so driving the LED afterwards is fine. The BOOT button
(GPIO9) is not used: held at power-on it starts the flasher. Pins, the OLED
address and the time zone (only for showing times) are in `idf.py
menuconfig` → "Satellite board".

## Building and flashing

No local ESP-IDF needed; the official image does it:

```sh
cd firmware
docker run --rm -v $PWD:/project -w /project/esp-idf/satellite-c6 \
  -u $(id -u):$(id -g) -e HOME=/tmp espressif/idf:v6.1.1 idf.py build

# flash over USB (the board shows up as /dev/ttyACM0)
docker run --rm -v $PWD:/project -w /project/esp-idf/satellite-c6 \
  --device /dev/ttyACM0 -u $(id -u):$(id -g) --group-add $(stat -c %g /dev/ttyACM0) \
  -e HOME=/tmp espressif/idf:v6.1.1 idf.py -p /dev/ttyACM0 flash
```

CI builds the same and keeps the whole flash image as the artifact
`lz-satellite-c6` (`lz-satellite-c6-full.bin`, to be written at `0x0`:
`esptool --chip esp32c6 write-flash 0x0 lz-satellite-c6-full.bin`).

## Setup (USB serial, until BLE)

A satellite without a state shows "Setup" and a purple light, and waits
for the setup on its USB serial port. On a full device with `lz`:

```sh
stty -F /dev/ttyACM0 raw -echo
lz satellite add Kitchen --wifi-name 'Home' --wifi-password '…' > /dev/ttyACM0
```

`lz` prints the setup as hex; the satellite takes it as soon as what has
arrived forms a setup, stores it, answers `lz-setup: ok` and connects. The
setup holds keys: send it only over the cable, and keep the WiFi password
out of the shell history (a leading space with `HISTCONTROL=ignorespace`,
or read it into a variable). Nothing is logged while the satellite waits,
so that a terminal that echoes cannot feed log lines back in.

## Using it

| | |
|---|---|
| Answer button, short | Answers every open request (`response`, manual). Otherwise wakes the display and ends the shown answer to an own ask. |
| Answer button, 1.5 s | The preset state (from the full device's satellite settings), or ends it |
| Ask button | Asks the contact bound to button 1; "Not yet … from 18:00" if the rules forbid it now |
| Answer button held at power-on | Countdown; held for 10 s everything is erased (factory reset) |

The OLED shows the time and the connection on top, then who is asking
("Mum wants a sign of life", more names below), answers to own asks
("✓ Mum 14:02", the reason of a reported one), the state that applies
("Asleep until 07:00") and notes for a few seconds after a press. With
nothing to show it goes dark after a minute.

| Light | Meaning |
|---|---|
| Off | Nothing open |
| Blue, slow pulse | Someone is asking |
| Green flash, 2 s | Answered, or asked |
| Green, steady dim | An own ask was answered (until the next press, at most 24 h) |
| Teal, steady dim | An own ask got a reported answer |
| Yellow, steady dim | A state is set |
| Red, blinking | No WiFi or relay for a minute, clock off, or access lost |
| Purple | Setup, reset |

Texts on the device are English for now.

## Flash encryption

Secure boot and flash encryption with NVS encryption are the recommended
setup (`docs/m10-satellite.md`, decision 4): without them, whoever holds
the board can read its keys from the flash. They burn eFuses for good, so
the defaults leave them off; the partition table already has the
`nvs_keys` partition. To turn them on for a device in use, set in
`menuconfig` "Security features" → "Enable flash encryption on boot"
(release mode) and "Enable NVS encryption", and read Espressif's flash
encryption guide first: a device in release mode can only be flashed
again over the air (not built yet).

## The adapter

```c
#include "lz_esp.h"

nvs_flash_init();
if (!lz_esp_has_state())
    lz_esp_setup_serial(progress, NULL);  /* blocks until set up */
lz_esp_hooks hooks = { .view = …, .led = …, .event = …, .status = …, .tick = …, .erased = … };
lz_esp_start(&hooks);                     /* WiFi, NTP, the task */
/* from button handlers: */
lz_esp_press(); lz_esp_long_press(); lz_esp_ask(1);
```

One task owns the `lz_ctx` and carries out the core's outputs in order;
WebSocket and HTTP callbacks only queue messages for it. Each relay gets
its own WebSocket (pings every 30 s, no automatic reconnect: the core
reconnects with backoff); HTTP runs one request at a time in its own task
and reports the `Date` header. The state blob lives in NVS (namespace `lz`,
at most `lz_save_max()` bytes, hence the 192 KB NVS partition); the WiFi
password stays in it, WiFi runs with RAM storage. The hooks run in the
task with the context read-only. Menuconfig → "Lebenszeichen": NTP server,
task stack, `http://` relays for development.
