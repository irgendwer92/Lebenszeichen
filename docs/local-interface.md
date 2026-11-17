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
- **Only on the device that received it, unless you share it.** A state
  set on the laptop does not travel to the phone. Report it to the device
  that should answer, or use the option `share` (see "Sharing a state with
  your devices").
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
| `GET /state` | – | `{"reason":…,"until":…,"shared":…,"handed":…}` (`until` in Unix seconds), or 404 |

Minutes: 1 to 10080 (one week). The reason must not be empty and is at
most 64 bytes.

## Sharing a state with your devices

With `"share":true` in the body of `POST /state`, or `?share=true` on
`DELETE /state`, the state goes to all your own devices, not only this one.
Without it, everything works as described above.

- **How it travels.** Through the encrypted sync mailbox of your devices.
  Contacts and the relay see nothing new.
- **Who answers.** Each device answers only for the contacts where
  reported answers are allowed *on that device*, after its own waiting
  time. A contact then sees the reason, marked as reported, from
  whichever device answers.
- **A newer shared state replaces the shared one** on all devices.
- **Ending it.** `DELETE /state?share=true` (`lz state clear --share`)
  ends a shared or handed state on all devices. A state that was never
  shared stays: a bed sensor's "asleep" on the laptop survives a "Phone
  empty" being cleared. Plain `DELETE /state` ends only the state set on
  this device; a state handed over by another device stays. ✕ in the app
  ends a shared or handed state everywhere (a state that was never shared:
  only here).
- **`GET /state`** says `"shared":true` for an own state that was shared,
  and `"handed":true` for one received from another device. In the app
  these show as "(shared)" and "(from another device)".
- With one device only, `share` just sets the state here. Adding a device
  does not take states along.
- Any other query on `DELETE` is an error (400).

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

# share with all your devices; clear everywhere
curl --fail-with-body --unix-socket "$sock" \
     -d '{"reason":"Phone empty","minutes":1440,"share":true}' http://lz/state
curl --fail-with-body --unix-socket "$sock" -X DELETE 'http://lz/state?share=true'
```

With the example script `examples/report-state.sh` (needs curl, uses jq
if present):

```sh
examples/report-state.sh set 480 asleep
examples/report-state.sh clear
examples/report-state.sh show
examples/report-state.sh set --share 1440 Phone empty
examples/report-state.sh clear --share
LZ_SOCKET=$XDG_RUNTIME_DIR/lebenszeichen/lz.sock examples/report-state.sh show
```

With `lz`, for the device of `lz listen`:

```sh
lz state set 480 asleep
lz state clear
lz state
lz state set --share 1440 Phone empty
lz state clear --share
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
.\report-state.ps1 set -Share 1440 Phone empty
.\report-state.ps1 clear -Share
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
| Extras | `token` (both), `reason` and `minutes` (only `SET_STATE`), `share` (optional, both) |

Every extra may be text; `minutes` may also be a number, `share` may be
the text `true` or a Boolean (anything else counts as false). With `share`,
the state goes to all your devices (`SET_STATE`) or ends on all of them
(`CLEAR_STATE`). A broadcast with
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

To hand the state to all your devices, add `share:true:String`.

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

This only works while the phone is reachable for Home Assistant. The
add-on below answers without the phone.

## Recipe: "Phone empty"

When the battery of your phone is nearly empty, it can hand over the state
"Phone empty" to your other devices, so that your contacts see
**“Phone empty” (reported)** from another device while the phone is off.
Once the phone is charged, the state ends on all devices. Allow reported
answers for the contacts in question on the devices that should answer.

**a) The Android setting.** In the app, Settings: **"Hand over a state when
the battery is low"** (off by default). At 2% or less the phone sets and
shares "Phone empty" for 24 hours. At 5% or more, or as soon as it charges,
it ends the state on all devices. It does this once per crossing: after
sharing it waits for 5% or charging, so a ✕ on another device is not undone
by the next check. The level is checked about every 10 minutes and at every
fetch; under Doze or battery saver this may be late.

**b) Home Assistant.** Either way, trigger on the battery level of the phone
(the Companion app's battery level sensor, `sensor.<phone>_battery_level`),
for example below 3 and above 5 for the way back.

- A broadcast to the phone, as above, with `share:true:String` and the
  reason `Phone empty` and `minutes:1440`:

  ```yaml
  intent_extras: "token:<your token>:String,reason:Phone empty:String,minutes:1440,share:true:String"
  ```

  The space in the reason is fine; `,`, `:` and `;` still need the
  URL-encoding described above. To end it: `CLEAR_STATE` with the token and
  `share:true:String`. This needs the phone to be reachable, so it can be
  late when the battery is almost gone.
- Without the phone: `shell_command` with `curl` on the socket of the
  add-on (see the section below), which hands the state over to all
  devices from the server:

  ```yaml
  # secrets.yaml
  lz_phone_empty: >-
    curl -sS --fail-with-body --unix-socket /share/lebenszeichen/lz.sock
    -H "Authorization: Bearer <token>" -H "Content-Type: application/json"
    -d '{"reason":"Phone empty","minutes":1440,"share":true}' http://lz/state
  lz_phone_charged: >-
    curl -sS --fail-with-body --unix-socket /share/lebenszeichen/lz.sock
    -H "Authorization: Bearer <token>" -X DELETE 'http://lz/state?share=true'
  ```

## Docker and Home Assistant

`lz-server` (image `ghcr.io/irgendwer92/lebenszeichen`) offers the same
interface without a phone or desktop app: a local socket with token,
optionally an HTTP port (token, plain HTTP: trusted network only), and in
Home Assistant a setup page. Plain Docker, see
`deploy/docker/compose.yaml`:

```yaml
services:
  lebenszeichen:
    image: ghcr.io/irgendwer92/lebenszeichen:latest
    restart: unless-stopped
    user: "1000:1000"   # uid of the host user who runs the scripts
    volumes:
      - ./data:/data
      - ${XDG_RUNTIME_DIR}/lebenszeichen-docker:/run/lebenszeichen
```

Set up once with `docker compose exec -it lebenszeichen lz device join
--relay https://RELAY --name Server`. The Home Assistant app (sidebar page,
socket `/share/lebenszeichen/lz.sock`, `shell_command` example) is described
in `deploy/home-assistant/lebenszeichen/DOCS.md`. Technical details:
`m8b-container.md`.

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
