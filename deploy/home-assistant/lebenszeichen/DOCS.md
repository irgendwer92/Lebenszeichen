# Lebenszeichen

This app lets your Home Assistant tell Lebenszeichen that you are "asleep"
(or any other short state). If someone asks while you can't answer, the app
answers for you after a waiting time. They see **“asleep” (reported)**.

## Install

1. In Home Assistant open **Settings → Apps → Install app → ⋮ → Repositories**
   (older versions: **Add-ons → Add-on store**) and add
   `https://github.com/irgendwer92/Lebenszeichen`.
2. Install **Lebenszeichen**, start it, and turn on **Show in sidebar**.

## Set up

1. Open **Lebenszeichen** in the sidebar.
2. Add the app as a device of your person: enter the relay address, a name,
   and follow the steps. Compare the check code with the one on your other
   device, and only then confirm.
3. For each contact that may get reported answers, turn on "reported
   answers" and set the waiting time. This is done only on this page.
4. Copy the **token** from the page. You can
   renew it there; then update your automations.

Put the token in `secrets.yaml`, not in an automation.

## Report a state from an automation

The socket is `/share/lebenszeichen/lz.sock`. `shell_command` runs in the
Home Assistant container, as root, and reaches it; `curl` is there. Put the
whole command into `secrets.yaml`, because `!secret` replaces only whole
values (this is the way that was tested):

```yaml
# secrets.yaml
lz_asleep: >-
  curl -sS --fail-with-body --unix-socket /share/lebenszeichen/lz.sock
  -H "Authorization: Bearer <token>" -H "Content-Type: application/json"
  -d '{"reason":"asleep","minutes":600}' http://lz/state
lz_clear: >-
  curl -sS --fail-with-body --unix-socket /share/lebenszeichen/lz.sock
  -H "Authorization: Bearer <token>" -X DELETE http://lz/state
```

```yaml
# configuration.yaml
shell_command:
  lz_asleep: !secret lz_asleep
  lz_clear: !secret lz_clear

automation:
  - alias: Lebenszeichen asleep
    triggers:
      - trigger: state
        entity_id: input_boolean.sleep_mode
        to: "on"
    actions:
      - action: shell_command.lz_asleep
        response_variable: result
      - if: "{{ result.returncode != 0 }}"
        then:
          - action: persistent_notification.create
            data:
              message: "Lebenszeichen: {{ result.stderr }} {{ result.stdout }}"
```

After a new token (Renew on the page), change it in `secrets.yaml` and
restart Home Assistant.

A failing command is not an error for Home Assistant: check `returncode`
yourself. `shell_command` stops a command after 60 seconds.

The calls are the same as in `docs/local-interface.md` (`POST /state`,
`DELETE /state`, `GET /state`).

## Options

- **relay**: relay address offered on the setup page. Optional.
- **http**: turns on the same interface over HTTP on port 8080. Off by
  default. Also set a host port under **Network**.

> **Warning:** plain HTTP sends the token in clear text. Anyone who can read
> the traffic can report states for you. Turn it on only in a network you
> trust, and never open the port to the internet.

## Backups

The app's data (`/data`) holds the keys of your device and contacts, and the
token. Backups contain them: set a **backup password** and store backups
safely.
