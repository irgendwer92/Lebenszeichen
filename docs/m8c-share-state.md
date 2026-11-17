# M8c – Handing a state to the own devices

Done when: the phone hands over "Phone empty" at low battery, the phone is
switched off, a contact asks and sees "“Phone empty” (reported)" from
another device after the waiting time; once the phone is charged again the
state is cleared on all devices (roadmap).

Builds on M8 (`m8-local-interface.md`, guide `local-interface.md`) and
M8b (`m8b-container.md`).

## Decisions (2026-11-17)

1. Two new sync messages, `state_shared` and `state_cleared`, in the sync
   mailbox (form C). Contacts and the relay see nothing new.
2. Two slots per device: the own state (set through this device's
   interface) and the handed state (received from another device). The
   valid one set last applies. A received `state_cleared` removes the
   handed state and an own state only if that one was shared; an own state
   that was never shared (a bed sensor's "asleep") stays.
3. ✕ on a shared or handed state clears it on all devices (sends
   `state_cleared`); ✕ on an own state that was never shared clears only
   this device.
4. Option `share` on the socket, the pipe, the Android intent, `lz state`
   and the example scripts. With one device only, `share` just sets the
   state here.
5. Android reads the battery level itself (`BatteryManager`, no
   permission): setting "Hand over a state when the battery is low",
   off by default. At 2% or less it sets and shares "Phone empty" for 24
   hours; at 5% or more, or as soon as it charges, it clears it on all
   devices. It fires once per crossing: after a share it waits for 5% or
   charging before it can share again, so a ✕ elsewhere is not undone by
   the next check. Checked on every fetch and wake, at app start and boot,
   and by a job every 10 minutes (a one-time WorkManager job that plans
   the next one, as periodic jobs cannot run more often than every 15
   minutes). Under Doze and battery saver the job may run late.
6. The trigger may also be Home Assistant: a `command_broadcast_intent`
   with `share` to the phone, or `curl` on the add-on's socket with
   `share`, which does not need the phone at all. Both go into
   `local-interface.md`.
7. Adding a device still does not take states along.

## Wire format

| Type | Name | Body |
|---|---|---|
| 71 | `state_shared` | length of the reason (1), reason (at most 64 bytes UTF-8), end of the state (8, Unix seconds) |
| 72 | `state_cleared` | – |

- Applied only if newer than the last `state_shared` or `state_cleared`
  this device applied: later sending time, or the same sending time and a
  larger message identifier (bytewise), as for `rules_set`. A delayed old
  `state_shared` cannot bring back a cleared state.
- A `state_shared` whose end has passed is ignored (but still counts as
  the newest stamp).
- Older clients ignore the unknown types.

## Interface

| Call | Body | Answer |
|---|---|---|
| `POST /state` | `{"reason":"Phone empty","minutes":1440,"share":true}` | 204; 400 with `{"error":…}` |
| `DELETE /state` | – | 204, clears the own state here |
| `DELETE /state?share=true` | – | 204, clears here and on all devices |
| `GET /state` | – | `{"reason":…,"until":…,"shared":…,"handed":…}` or 404 |

- `share` defaults to `false`; plain calls behave as in M8.
- `DELETE /state` without `share` clears only the own state; a handed
  state stays (use `share=true` to end it everywhere).
- Android: extra `share` on `SET_STATE` and `CLEAR_STATE`, `true` as text
  or Boolean; anything else counts as false.
- `lz state set [--share] <minutes> <reason>`, `lz state clear [--share]`.
- The shared message goes into the outbox; `lz listen`, the desktop app
  and `lz-server` send it within their 10-second tick, Android with an
  expedited job at once.

## Display

"Reported state: “Phone empty” until 14:00 (from another device) ✕" on
the desktop, on Android, on the add-on page and in `lz contacts`; "(shared)"
for an own shared state.

## Work

- Inline: wire (types 71, 72), core (two slots, stamps, `share`), client
  (receive, outbox, `local.rs`, `lz state`), tests.
- Then in parallel: Android (intent extra, battery setting and job, state
  line) and desktop window, add-on page and example scripts.
- Docs: `wire-format.md`, `protocol.md`, `local-interface.md`, roadmap.

## Test record

- [ ] Linux: `report-state.sh set --share …` shows the handed state on the
      phone and the add-on; `clear --share` clears it there.
- [ ] ✕ on a handed state clears it on all devices.
- [ ] A handed clear leaves an unshared own state alone.
- [ ] Phone: battery level faked over adb (`dumpsys battery set level 2`)
      hands over "Phone empty"; `level 6` / `set ac 1` clears it.
- [ ] Done test: real battery at 2%, phone switched off, the test contact
      asks and receives "“Phone empty” (reported)" from another device;
      after charging the state is cleared everywhere.
