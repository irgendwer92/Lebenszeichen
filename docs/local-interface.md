# Reporting a state: the local interface

Other programs can tell Lebenszeichen a state such as "asleep", for
example sleep tracking, home automation or a script that sees the CPAP
device running. If someone asks while you can't answer, your device
answers for you after a waiting time. They see **“asleep” (reported)**
instead of ✓.

Technical details and decisions are in `m8-local-interface.md`.

## How it works

- **A state** is a short reason and a duration: "asleep" for 480 minutes.
  It ends by itself afterwards, or earlier when it is cleared. A new state
  replaces the old one.
- **Only on the device that received it.** A state set on the laptop does
  not travel to the phone. Report it to the device that should answer.
- **Per contact, off by default.** Under ⚙ of a contact, section
  "Reported state", turn on "Answer … with my reported state from this
  device", and set after how many minutes without an answer (default 15).
  With `lz`: `lz state allow Anna --wait 15`, `lz state deny Anna`.
- **The waiting time starts when the request arrives on the device.** If
  you tap in time, the contact gets ✓ and no reported answer. If the
  reported answer went out, the request stays open: a later tap still
  sends ✓.
- **What the contact sees:** exactly your reason and the marker
  "(reported)". Never the remaining duration, nor when you set the state.
  Choose the reason with that in mind: "asleep" rather than "CPAP on".
- The main view of the app shows the current state at the top, with ✕ to
  clear it.

All interfaces speak the same three calls:

| Call | Body | Answer |
|---|---|---|
| `POST /state` | `{"reason":"asleep","minutes":480}` | 204, or 400 with `{"error":…}` |
| `DELETE /state` | – | 204 |
| `GET /state` | – | `{"reason":…,"until":…}` (Unix seconds), or 404 |

Minutes: 1 to 10080 (one week). The reason must not be empty and is at
most 64 bytes.

## Linux

The desktop app listens on `$XDG_RUNTIME_DIR/lebenszeichen/app.sock`,
`lz listen` on `…/lz.sock`. Only your user can reach them.

With curl:

```sh
sock=$XDG_RUNTIME_DIR/lebenszeichen/app.sock
curl --fail-with-body --unix-socket "$sock" \
     -d '{"reason":"asleep","minutes":480}' http://lz/state
curl --fail-with-body --unix-socket "$sock" -X DELETE http://lz/state
curl --unix-socket "$sock" http://lz/state
```

With the example script `examples/report-state.sh` (needs curl, uses jq
if present):

```sh
examples/report-state.sh set 480 asleep
examples/report-state.sh clear
examples/report-state.sh show
LZ_SOCKET=$XDG_RUNTIME_DIR/lebenszeichen/lz.sock examples/report-state.sh show
```

With `lz`, for the device of `lz listen`:

```sh
lz state set 480 asleep
lz state clear
lz state
```

### Recipe: asleep every night from 23:00

A systemd user timer, `~/.config/systemd/user/lz-asleep.service`:

```ini
[Service]
Type=oneshot
ExecStart=%h/bin/report-state.sh set 480 asleep
```

and `~/.config/systemd/user/lz-asleep.timer`:

```ini
[Timer]
OnCalendar=*-*-* 23:00
Persistent=false

[Install]
WantedBy=timers.target
```

Then `systemctl --user enable --now lz-asleep.timer`. The state ends by
itself after eight hours.

## Windows

The desktop app listens on the named pipe
`\\.\pipe\lebenszeichen-<your SID>-app`. Only your user can open it.
`lz` has no interface on Windows.

With the example script `examples/report-state.ps1`:

```powershell
.\report-state.ps1 set 480 asleep
.\report-state.ps1 clear
.\report-state.ps1 show
```

The script sends nothing if the pipe does not belong to you (another user
on the same PC could have taken the name).

### Recipe: asleep every night from 23:00

Task Scheduler → Create Task: trigger daily at 23:00, action "Start a
program" with

```
powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\path\to\report-state.ps1 set 480 asleep
```

## Android

Android apps cannot open a socket of another app, so here other apps send
a broadcast. It is protected by a token instead of a permission, because
automation apps cannot hold custom permissions.

1. In the app, open the device panel and turn on **"Accept state reports
   from other apps"**. A token appears.
2. Copy it into the automation app. Keep it secret: whoever has it can set
   your state. **Renew** gives a new token; the old one stops working.
   Turning the switch off deletes the token.
3. Allow reported answers for the contacts in question (⚙ of the contact).

The broadcast:

| | |
|---|---|
| Package | `io.github.irgendwer92.lebenszeichen` |
| Action | `io.github.irgendwer92.lebenszeichen.SET_STATE` or `….CLEAR_STATE` |
| Extras | `token` (both), `reason` and `minutes` (only `SET_STATE`) |

Every extra may be text; `minutes` may also be a number. A broadcast with
a wrong token, or while the switch is off, is dropped without a trace.

### Tasker

Task → add action **System → Send Intent**:

| Field | Value |
|---|---|
| Action | `io.github.irgendwer92.lebenszeichen.SET_STATE` |
| Extra | `token:<your token>` |
| Extra | `reason:asleep` |
| Extra | `minutes:600` |
| Package | `io.github.irgendwer92.lebenszeichen` |
| Class | (empty) |
| Target | Broadcast Receiver |

For clearing, a second task with Action `….CLEAR_STATE` and only the
token extra.

Example profile: Sleep as Android announces when sleep tracking starts
and stops; in Tasker use the corresponding event (see the Sleep as Android
documentation on Tasker integration) and run the two tasks as entry and
exit task. Or simpler: a time profile 23:00–07:00 with the set task on
entry and the clear task on exit.

### Automate

Block **Broadcast send**: Package `io.github.irgendwer92.lebenszeichen`,
Action `io.github.irgendwer92.lebenszeichen.SET_STATE`, Extras as a
dictionary:

```
{"token": "<your token>", "reason": "asleep", "minutes": 600}
```

### Home Assistant (Companion app)

Home Assistant tells the Companion app on your phone to send the
broadcast:

```yaml
action: notify.mobile_app_<phone>
data:
  message: command_broadcast_intent
  data:
    intent_package_name: io.github.irgendwer92.lebenszeichen
    intent_action: io.github.irgendwer92.lebenszeichen.SET_STATE
    intent_extras: "token:<your token>:String,reason:asleep:String,minutes:480"
```

Extras are `name:value` pairs separated by commas; `:String` keeps a value
as text. A reason with `,`, `:` or `;` must be URL-encoded and marked
`:String.urlencoded`. Clearing: `intent_action: ….CLEAR_STATE` and only
the token. Store the token in `secrets.yaml`, not in the automation.

Example automation:

```yaml
automation:
  - alias: Lebenszeichen asleep
    triggers:
      - trigger: state
        entity_id: input_boolean.sleep_mode
        to: "on"
    actions:
      - action: notify.mobile_app_phone
        data:
          message: command_broadcast_intent
          data:
            intent_package_name: io.github.irgendwer92.lebenszeichen
            intent_action: io.github.irgendwer92.lebenszeichen.SET_STATE
            intent_extras: !secret lz_extras_asleep
```

with `lz_extras_asleep: "token:<your token>:String,reason:asleep:String,minutes:600"`
in `secrets.yaml`.

This only works while the phone is reachable for Home Assistant. A
container and an add-on for Home Assistant that answer without the phone
follow with M8b.

## Troubleshooting

- **The contact got nothing.** Check in this order: is a state set
  (top of the main view, `GET /state`)? Are reported answers allowed for
  this contact *on this device*? Has the waiting time since the request
  arrived passed? Was the app running then (Android: the job may run a
  little late in power saving)? Did you tap in time (then the contact got
  ✓ instead)?
- **Linux: `Nothing serves the socket`.** The app or `lz listen` is not
  running, or you use the socket of the other one (`app.sock` vs
  `lz.sock`).
- **Windows: "the pipe … belongs to …".** Another user holds the name;
  the script refuses on purpose. Sign that user out, then restart the app.
- **Android: nothing happens.** The switch is off, the token was renewed
  since, or the package or action has a typo. Wrong reports are dropped
  silently by design.
- **The token got out.** Renew it in the device panel, then update it in
  the automation app.
