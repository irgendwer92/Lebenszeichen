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

## Test record

(to be filled)
