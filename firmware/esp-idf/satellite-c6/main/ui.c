/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "board.h"
#include "display.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "ui.h"

#define SCREEN_OFF_S 60 /* with nothing to show, the OLED goes dark */
#define NOTE_S 5
#define WARN_GRACE_S 60 /* no light for a short connection loss */

static lz_esp_status s_st;
static int s_core_led;
static uint64_t s_offline_since;
static char s_note_big[48], s_note_small[64];
static uint64_t s_note_until;
static uint64_t s_active_at;
static volatile int s_wake;
static int s_screen_on = 1, s_last_minute = -1;

static uint64_t uptime_s(void)
{
    return (uint64_t)(esp_timer_get_time() / 1000000);
}

static void hhmm(char out[8], uint64_t t)
{
    time_t tt = (time_t)t;
    struct tm tm;
    localtime_r(&tt, &tm);
    snprintf(out, 8, "%02d:%02d", tm.tm_hour, tm.tm_min);
}

static void flush(void)
{
    if (disp_flush() != ESP_OK) {
        /* A loose OLED must not stop the satellite. */
    }
}

void ui_message(const char *big, const char *small)
{
    disp_on(1);
    disp_clear();
    disp_text_center(&font_large, 16, big, SIZE_MAX);
    if (small) disp_text_center(&font_small, 38, small, SIZE_MAX);
    flush();
}

void ui_setup(size_t bytes, int refused)
{
    char line[40];
    disp_on(1);
    disp_clear();
    disp_text_center(&font_large, 0, "Setup", SIZE_MAX);
    disp_hline(0, 16, DISP_W);
    if (bytes) {
        snprintf(line, sizeof(line), "Receiving %u bytes", (unsigned)bytes);
        disp_text_center(&font_small, 28, line, SIZE_MAX);
    } else if (refused) {
        snprintf(line, sizeof(line), "Setup refused (%d)", refused);
        disp_text_center(&font_small, 22, line, SIZE_MAX);
        disp_text_center(&font_small, 34, "Run lz satellite add", SIZE_MAX);
        disp_text_center(&font_small, 46, "again", SIZE_MAX);
    } else {
        disp_text_center(&font_small, 22, "Connect USB and run", SIZE_MAX);
        disp_text_center(&font_small, 34, "lz satellite add", SIZE_MAX);
        disp_text_center(&font_small, 46, "> /dev/ttyACM0", SIZE_MAX);
    }
    flush();
    board_light(LIGHT_SETUP);
}

void ui_wake(void)
{
    s_wake = 1;
}

static const char *name_of(const lz_ctx *ctx, uint8_t contact, size_t *len)
{
    const lz_cname *n = lz_contact_name(ctx, contact);
    *len = n ? n->len : 1;
    return n ? (const char *)n->b : "?";
}

static int offline(void)
{
    return !s_st.wifi || s_st.connected < s_st.relays;
}

static void light(const lz_view_t *v)
{
    int l = LIGHT_OFF;
    int warn = s_core_led == LZ_LED_WARNING ||
               (s_offline_since && uptime_s() - s_offline_since >= WARN_GRACE_S);
    if (warn) l = LIGHT_WARNING;
    else if (s_core_led == LZ_LED_REQUEST) l = LIGHT_REQUEST;
    else if (s_core_led == LZ_LED_ANSWER_MANUAL) l = LIGHT_ANSWER;
    else if (s_core_led == LZ_LED_ANSWER_REPORTED) l = LIGHT_REPORTED;
    else if (v->state_set) l = LIGHT_STATE;
    board_light(l);
}

static void render(const lz_ctx *ctx)
{
    lz_view_t v;
    lz_view(ctx, &v);
    light(&v);

    uint64_t up = uptime_s();
    int busy = v.n_open || v.n_answers || v.clock_warning || up < s_note_until || offline();
    if (busy || s_wake) s_active_at = up;
    s_wake = 0;
    int on = up - s_active_at < SCREEN_OFF_S;
    if (on != s_screen_on) {
        disp_on(on);
        s_screen_on = on;
    }
    if (!on) return;

    char t[8] = "--:--", line[96];
    disp_clear();
    uint64_t now = (uint64_t)time(NULL);
    if (now >= LZ_MIN_TIME) hhmm(t, now);
    disp_text(&font_small, 0, 0, t, SIZE_MAX, 40);
    const char *net = !s_st.wifi          ? "no WiFi"
                      : !s_st.time        ? "no time"
                      : s_st.connected < s_st.relays ? "no relay"
                                                     : "";
    disp_text(&font_small, DISP_W - disp_text_width(&font_small, net, SIZE_MAX), 0, net,
              SIZE_MAX, 80);
    disp_hline(0, 11, DISP_W);

    size_t len;
    const char *name;
    if (up < s_note_until) {
        disp_text_center(&font_large, 20, s_note_big, SIZE_MAX);
        disp_text_center(&font_small, 40, s_note_small, SIZE_MAX);
    } else if (v.clock_warning) {
        disp_text_center(&font_large, 18, "Clock off", SIZE_MAX);
        disp_text_center(&font_small, 36, "Sending nothing until", SIZE_MAX);
        disp_text_center(&font_small, 46, "the time is right", SIZE_MAX);
    } else if (v.n_open) {
        name = name_of(ctx, v.open[0], &len);
        disp_text(&font_large, 0, 15, name, len, DISP_W);
        disp_text(&font_small, 0, 31, "wants a sign of life", SIZE_MAX, DISP_W);
        if (v.n_open > 1) {
            size_t pos = (size_t)snprintf(line, sizeof(line), "+");
            for (uint8_t i = 1; i < v.n_open && pos < sizeof(line) - 2; i++) {
                const char *o = name_of(ctx, v.open[i], &len);
                if (len > sizeof(line) - pos - 2) len = sizeof(line) - pos - 2;
                line[pos++] = ' ';
                memcpy(line + pos, o, len);
                pos += len;
            }
            disp_text(&font_small, 0, 42, line, pos, DISP_W);
        }
        disp_text(&font_small, 0, 54, "Press to answer", SIZE_MAX, DISP_W);
    } else if (v.n_answers) {
        int y = 14;
        for (uint8_t i = 0; i < v.n_answers && y < 50; i++) {
            char at[8];
            hhmm(at, v.answers[i].at);
            name = name_of(ctx, v.answers[i].contact, &len);
            int x = disp_text(&font_large, 0, y, "\xE2\x9C\x93 ", SIZE_MAX, 14);
            x += disp_text(&font_large, x, y, name, len, DISP_W - x - 42);
            disp_text(&font_small, DISP_W - 30, y + 3, at, SIZE_MAX, 30);
            y += 15;
            if (v.answers[i].kind == LZ_KIND_REPORTED && v.answers[i].reason.len) {
                disp_text(&font_small, 14, y - 1, (const char *)v.answers[i].reason.b,
                          v.answers[i].reason.len, DISP_W - 14);
                y += 10;
            }
        }
    } else {
        disp_text_center(&font_large, 22, "All quiet", SIZE_MAX);
    }

    if (v.state_set && !v.clock_warning && up >= s_note_until) {
        char until[8];
        hhmm(until, v.state_until);
        int n = snprintf(line, sizeof(line), "%.*s until %s", (int)v.state_reason.len,
                         (const char *)v.state_reason.b, until);
        if (n > (int)sizeof(line) - 1) n = (int)sizeof(line) - 1;
        if (!v.n_open) disp_text(&font_small, 0, 54, line, (size_t)n, DISP_W);
    }
    flush();
}

static void note(const char *big, const char *small)
{
    snprintf(s_note_big, sizeof(s_note_big), "%s", big);
    snprintf(s_note_small, sizeof(s_note_small), "%s", small ? small : "");
    s_note_until = uptime_s() + NOTE_S;
}

/* ---- hooks ---- */

static void h_view(void *user, const lz_ctx *ctx)
{
    (void)user;
    render(ctx);
}

static void h_led(void *user, int led)
{
    (void)user;
    s_core_led = led;
}

static void h_event(void *user, const lz_ctx *ctx, const lz_event *ev)
{
    (void)user;
    char small[64], at[8];
    int nl = ev->name.len;
    const char *nm = (const char *)ev->name.b;
    switch (ev->type) {
    case LZ_EV_ANSWERED:
        board_light(LIGHT_FLASH);
        snprintf(small, sizeof(small), "to %.*s", nl, nm);
        note(ev->kind == LZ_KIND_REPORTED ? "Reported" : "\xE2\x9C\x93 Answered", small);
        break;
    case LZ_EV_ASKED:
        board_light(LIGHT_FLASH);
        snprintf(small, sizeof(small), "%.*s", nl, nm);
        note("Asked", small);
        break;
    case LZ_EV_ASK_REFUSED:
        if (ev->contact == 0xFF) {
            note("No contact", "on this button");
        } else if (ev->at == UINT64_MAX) {
            snprintf(small, sizeof(small), "%.*s: not at all", nl, nm);
            note("Not possible", small);
        } else {
            hhmm(at, ev->at);
            snprintf(small, sizeof(small), "%.*s: from %s", nl, nm, at);
            note("Not yet", small);
        }
        break;
    case LZ_EV_OUTBOX_FULL:
        note("Not sent", "Too much waiting to go out");
        break;
    case LZ_EV_CONTACT_ADDED:
        snprintf(small, sizeof(small), "%.*s", nl, nm);
        note("New contact", small);
        break;
    case LZ_EV_CONTACT_REMOVED:
    case LZ_EV_CONTACT_ENDED:
        snprintf(small, sizeof(small), "%.*s", nl, nm);
        note("Contact gone", small);
        break;
    case LZ_EV_ACCESS_LOST:
        note("Access lost", ev->contact == 0xFF ? "to the phone" : "to a contact");
        break;
    case LZ_EV_NO_READER_TOKENS:
        note("Relay too old", "Update the relay");
        break;
    default:
        return;
    }
    s_wake = 1;
    render(ctx);
}

static void h_status(void *user, const lz_esp_status *st)
{
    (void)user;
    s_st = *st;
    if (offline()) {
        if (!s_offline_since) s_offline_since = uptime_s() ? uptime_s() : 1;
    } else {
        s_offline_since = 0;
    }
}

static void h_tick(void *user, const lz_ctx *ctx)
{
    (void)user;
    uint64_t now = (uint64_t)time(NULL);
    int minute = (int)(now / 60);
    uint64_t up = uptime_s();
    /* Redraw for the clock, a note running out, the screen timeout, a
     * press that woke the display, the warning grace. */
    if (minute != s_last_minute || s_wake || up == s_note_until ||
        up - s_active_at == SCREEN_OFF_S ||
        (s_offline_since && up - s_offline_since == WARN_GRACE_S) || !s_screen_on) {
        s_last_minute = minute;
        render(ctx);
    }
}

static void h_erased(void *user)
{
    (void)user;
    board_light(LIGHT_SETUP);
    ui_message("Removed", "Everything erased");
    vTaskDelay(pdMS_TO_TICKS(4000));
}

void ui_hooks(lz_esp_hooks *h)
{
    memset(h, 0, sizeof(*h));
    h->view = h_view;
    h->led = h_led;
    h->event = h_event;
    h->status = h_status;
    h->tick = h_tick;
    h->erased = h_erased;
}
