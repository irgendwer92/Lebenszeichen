# Security policy

Lebenszeichen is meant to be relied on when someone is not doing well, so
security reports are welcome.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** in this repository (private
vulnerability reporting). Include what is affected (app, `lz`, relay,
container), the version or commit, and how to reproduce it.

You get an answer within a week. Once a fix is released, the report is
published with credit, unless you prefer otherwise.

## Scope

- The encryption and protocol (`crates/wire`, `crates/core`), the relay
  (`crates/relay`), the clients (`crates/client`, `crates/cli`, `app/`) and
  the container (`crates/server`, `deploy/`).
- What the design accepts as known limits is listed in
  [docs/review-brief.md](docs/review-brief.md); reports that make one of
  them worse are in scope.

Only the latest release and `main` get fixes.
