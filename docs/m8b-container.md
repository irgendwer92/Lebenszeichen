# M8b – Container and Home Assistant

Done when: a Home Assistant automation and a host script each report
"asleep", and the requester sees "“asleep” (reported)" (roadmap).

Builds on M8 (`m8-local-interface.md`, guide `local-interface.md`).

## Decisions (2026-11-17)

1. The image is built in CI and published on ghcr.io
   (`ghcr.io/irgendwer92/lebenszeichen`); the Home Assistant app pulls it
   and builds nothing. amd64 first, aarch64 later.
2. Setup and settings through a small web page, in Home Assistant as an
   ingress page in the sidebar: add this as a device of the person (device
   code as QR and text, compare the check code, confirm), the setting per
   contact (reported answers allowed, waiting time), the state with ✕, the
   token. Nothing else: no asking, inviting or removing.
3. The setting per contact is changed only on that page (or `lz state
   allow` in plain Docker), not through app options.
4. The token for the socket is generated at the first start, shown on the
   page and in the log, and can be renewed there.
5. The optional HTTP port for the same interface is off by default, needs
   the token, and comes with a warning that plain HTTP exposes the token on
   the network.

## Components

- `crates/server` (`lz-server`): one process with `listen`, the local
  socket (with token), optionally the HTTP port (with token), and the page
  with its JSON API.
- The page: static HTML/JS embedded in the binary, relative URLs only (the
  ingress path differs per installation), English and German.
- Docker image: multi-stage, Debian slim with `ca-certificates`; state in
  `/data`, socket directory as a volume. CI workflow for ghcr.io on `v*`
  tags and by hand.
- Home Assistant app in this repository: `repository.yaml` at the root, the
  app in its own folder; `ingress: true`, `map: share:rw`, option
  `http_port`.

## `lz-server`

```
lz-server [--state PATH]            state file (env LZ_STATE), default /data/device.json
          --socket PATH             local interface, always with token
          [--http ADDR]             same interface over TCP, token, warning in the log
          [--page ADDR --page-allow IP ...]
                                    the page; refuses every peer not listed
          [--relay URL]             default relay for joining
```

- The token is created at the first start if there is none, written to
  the log once, and kept in the state file (`Reports.token`).
- Without a state file only the page runs (setup). After joining, the
  process listens like `lz listen` and sends due reported answers; its
  notices go to standard output (the app log in Home Assistant).
- In Home Assistant: `--socket /share/lebenszeichen/lz.sock --page
  0.0.0.0:8099 --page-allow 172.30.32.2`. Plain Docker: no page by
  default; setup with `docker exec -it lebenszeichen lz device join …`.

## Page API

Relative URLs under the page (`api/…`), JSON, errors as
`{"error": "<text in the language>"}` with 4xx/5xx.

| Call | Body | Answer |
|---|---|---|
| `GET api/texts` | – | `{"lang":"en","texts":{"web-…":"…"}}` (raw `web-` texts, `{ $var }` placeholders) |
| `GET api/status` | – | see below |
| `POST api/join` | `{"relay":"https://…","name":"Home Assistant"}` | 202; the flow runs in the background |
| `POST api/join/answer` | `{"matches":true}` | 204 |
| `POST api/join/cancel` | – | 204 |
| `PUT api/contacts/<name>/reports` | `{"allowed":true,"delay_minutes":15}` | 204 (name URL-encoded) |
| `DELETE api/state` | – | 204 |
| `POST api/token/renew` | – | `{"token":"<hex>"}` |

`GET api/status`:

```json
{
  "setup": "none | joining | ready",
  "default_relay": "https://… or null",
  "join": null,
  "device": {"name": "Home Assistant", "others": ["Phone", "Laptop"]},
  "state": {"reason": "asleep", "until": 1794962903, "text": "Reported state: “asleep” until today 07:00"},
  "contacts": [
    {"name": "Anna", "paired": true, "reports_allowed": false,
     "report_delay_minutes": 15, "incoming": "Anna has been asking since … or null"}
  ],
  "token": "<64 hex digits>",
  "socket": "/share/lebenszeichen/lz.sock",
  "http": null
}
```

While joining, `join` is
`{"step": "device_code | check_device | confirm_device | check_handover | confirm_handover | done | failed",
"code": "…", "qr": {"width": 33, "dark": [true, …]}, "progress": ["…"], "error": null}`
(`qr` only with `device_code`, same format as `lz_client::qr::Qr`).
`confirm_*` waits for `api/join/answer`. `device` is null before setup;
`http` is `{"address": "0.0.0.0:8080"}` when the port is on.

## Facts checked against the Home Assistant docs (2026-11-17)

- Ingress requests come only from the Supervisor at `172.30.32.2`; the app
  must refuse everything else. Default port 8099. Home Assistant handles
  the login; the page must use relative URLs.
- The Supervisor finds apps at any depth of a repository with a
  `repository.yaml` (only `name` required). The docs now say "apps"
  instead of "add-ons".
- `shell_command` runs as root inside the `homeassistant` container, is
  stopped after 60 seconds, and returns `stdout`, `stderr` and
  `returncode`; a non-zero exit code is no error, so automations check
  `returncode`.

## Test record (2026-11-17)

- [x] Image built locally (`docker build -f deploy/docker/Dockerfile .`),
      run as uid 1000 with the page on `127.0.0.1` (`--page-allow
      127.0.0.1`): socket 0600 in a 0700 directory; `401` without a state
      file; page with the strict headers; `api/status` before setup.
- [x] Joined from the page as the device "testcontainer" of the user's
      person: device code, both check codes, contacts handed over; the
      token was written to the log once. Per-contact setting on the page.
- [x] Host script: `examples/report-state.sh set 30 container` with
      `LZ_TOKEN` on the container's socket (401 without it); a request of
      the test contact (21:35:24) was answered with "“container”
      (reported)" (21:36:33).
- [x] With a second device that still had a valid state, only one
      reported answer went out; the container logged "Reported on another
      device" and sent none.
- Found: the image had no time zone data (times in UTC); `tzdata` added,
  `TZ` in the compose example.
- [x] Image published by the workflow (`ghcr.io/irgendwer92/lebenszeichen:0.1.0`),
      pulled anonymously.
- [x] Home Assistant OS (amd64): repository
      `https://github.com/irgendwer92/Lebenszeichen#m8b-container` added,
      app installed and started, joined from the ingress page as a device
      of the user's person. `shell_command` with the whole curl command in
      `secrets.yaml` (`!secret` replaces only whole values) set the state
      "ha" on `/share/lebenszeichen/lz.sock`; curl exists in the Home
      Assistant container. A request of the test contact (22:00:29) was
      answered with "“ha” (reported)" (22:01:38).
- [x] Removing the device "testcontainer" from another device: the
      container logged "This device was removed." and deleted its state
      file.
