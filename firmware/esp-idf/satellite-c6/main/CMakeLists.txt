idf_component_register(
  SRCS main.c board.c display.c ui.c font_small.c font_large.c
  INCLUDE_DIRS .
  PRIV_REQUIRES lebenszeichen nvs_flash esp_driver_gpio esp_driver_i2c esp_timer)
target_compile_options(${COMPONENT_LIB} PRIVATE -Wall -Wextra -Werror)
