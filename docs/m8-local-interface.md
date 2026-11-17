# M8 – Local interface

Done when: a script reports "asleep" and the requester sees
"“asleep” (reported)" after the waiting time (roadmap).

## Decisions (2026-11-17)

1. Android: no own permission. An exported receiver behind a switch
   "Accept state reports from other apps" (default off) that requires a
   random token as an extra; the token can be renewed. Reason: the apps
   that would report (Tasker, Automate, the Home Assistant Companion app)
   cannot hold custom permissions; sleep and CPAP apps do not call other
   apps.
2. Per contact and per device: "reported answers allowed from this device"
   (default off) and the waiting time (default 15 minutes). Not synced; a
   state applies only to the device that received it, and only that device
   decides.
3. The waiting time starts when the request arrives on the device, not at
   its sending time, so a device that was offline still gives the full
   waiting time for a manual answer.
4. The desktop app and Android show the current state ("Reported state:
   “asleep” until 07:00 ✕"); ✕ clears it. `lz contacts` shows it too.
5. The socket speaks minimal HTTP, so `curl --unix-socket` works from
   scripts, containers and Home Assistant. No network port in M8. M8b
   uses the socket through a bind mount; an HTTP port there is optional,
   off by default, with a token and a warning about plain HTTP.

## Interface

| Call | Body | Answer |
|---|---|---|
| `POST /state` | `{"reason":"asleep","minutes":480}` | 204; 400 with `{"error":…}` |
| `DELETE /state` | – | 204 |
| `GET /state` | – | `{"reason":…,"until":…}` or 404 |

- Reason: same limits as `lz report` (not empty, `Reason` length).
  Minutes: 1 to 10080 (one week). A new state replaces the old one.
- Linux: `$XDG_RUNTIME_DIR/lebenszeichen/app.sock` (desktop app) and
  `…/lz.sock` (`lz listen`), mode 0600; `--socket` sets another path.
  No token.
- Windows: named pipe for the current user only (DACL with the user's
  SID, remote clients rejected), same HTTP. Client: `lz state` or a
  PowerShell example.
- Android: explicit broadcast to the app, actions `SET_STATE` and
  `CLEAR_STATE`, extras `token`, `reason`, `minutes`. Wrong or missing
  token, or the switch off: dropped silently.
- `lz state set <minutes> <reason>`, `lz state clear`, `lz state`:
  client for the socket of `lz listen`.

## How it works

- Local state in the state file: the state (reason, until), and per
  contact the switch and the waiting time. Never part of the device
  handover or the group sync.
- For each incoming request the device notes when it arrived.
- Due: arrival + waiting time. At or after that moment, if the request is
  still open and not answered or reported, the contact allows it and a
  state is valid, the device sends `reported` and tells its other
  devices (as `lz report` does today). A state set after the moment is
  still used while the request is open.
- Desktop and `lz listen`: checked after every fetch and in the listen
  tick (every 10 seconds), so a newly set state is used within that tick;
  the interface only writes the state file and stays independent of the
  fetch loop. A failed delivery is rolled back and tried again; the
  problem is shown once per run. Android: a WorkManager job with the delay when a request
  arrives, and a check when a state is set; under Doze it may run late.

## Test record

(to be filled)
