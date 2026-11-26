# SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception
#
# Lebenszeichen satellite for the ESP32-C6 SuperMini with an SSD1306 OLED
# (128x64, I2C), an answer button, an ask button and the onboard RGB LED.
cmake_minimum_required(VERSION 3.22)
set(EXTRA_COMPONENT_DIRS ../lebenszeichen)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(lz-satellite-c6)
