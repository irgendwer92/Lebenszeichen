# Running your own relay

This guide sets up a relay for Lebenszeichen and the ntfy server that wakes
phones, and then pairs two people. It assumes you can use a Linux shell and
Docker; it assumes nothing about Lebenszeichen.

The relay passes encrypted messages between devices and keeps them until they
are picked up, for at most 24 hours. It sees only ciphertext of a fixed length,
random mailbox identifiers and the times messages arrive. ntfy delivers the
wake signals through which the relay starts a phone's app when it is closed
(UnifiedPush). The relay and ntfy can share one server.

## 1. What you need

- A server that is reachable from the internet on ports 80 and 443, with
  Docker and Docker Compose (`docker compose version`).
- Two DNS names pointing to it, one for the relay and one for ntfy. This
  guide writes `relay.example.org` and `ntfy.example.org`; replace them
  everywhere.
- One of these reverse proxies on the server, for TLS: **Caddy** (simplest:
  gets certificates by itself), **nginx** (with certbot) or an existing
  **Traefik**. Examples for all three are in `deploy/relay/proxy/`.
- `openssl` to create the access token.

**Both names must be reachable from the internet**, not only from your home
network. The relay sends wake signals only to public addresses, and a phone
on the road must reach ntfy from outside anyway. If the relay and ntfy run on
a home server and your router cannot route connections to its own public
address back in (NAT loopback, also called hairpinning), a local DNS entry
with the private address does not help: the relay refuses private addresses.
Turn on NAT loopback in the router instead, or run the server elsewhere.

## 2. Get the files

Copy the directory `deploy/relay/` of this repository to the server, for
example to `~/lebenszeichen`:

```sh
git clone https://github.com/irgendwer92/lebenszeichen.git
cp -r lebenszeichen/deploy/relay ~/lebenszeichen-relay
cd ~/lebenszeichen-relay
```

All commands below run in this directory. The compose project is named
`lebenszeichen`, so its volumes are `lebenszeichen_relay-data` and
`lebenszeichen_ntfy-data`.

It contains:

| File | Purpose |
|---|---|
| `compose.yaml` | relay and ntfy, listening only on `127.0.0.1` |
| `relay.env.example` | settings of the relay |
| `ntfy/server.yml` | settings of ntfy |
| `proxy/Caddyfile`, `proxy/nginx.conf`, `proxy/compose.traefik.yaml` | reverse proxy examples |

## 3. The access token

Whoever holds the access token can create mailboxes on your relay: set up
devices and invite others with a voucher. People you invite with a voucher
do not need it. Create it and put it into `relay.env`:

```sh
cp relay.env.example relay.env
chmod 600 relay.env
sed -i "s/^LZ_ACCESS_TOKEN=.*/LZ_ACCESS_TOKEN=$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=')/" relay.env
```

The same `sed` line replaces the token later (section 11).

Keep it like a password. You need it once more in step 7; after that, the
app can show it to your own further devices as a setup code.

Recommended: `LZ_PUSH_ALLOWED_HOSTS=ntfy.example.org` in `relay.env` makes
the relay send wake signals only to your own ntfy. Without it, any public
https address on port 443 is allowed, so that phones using another ntfy
server work too, but then anyone with a mailbox on your relay (everyone you
invited with a voucher) can make it send many requests to an address of
their choice. Set it unless people on your relay use other ntfy servers;
several hosts are separated by commas. `relay.env.example` sets it already:
replace the name, or remove the line.

## 4. Start relay and ntfy

The image `ghcr.io/irgendwer92/lebenszeichen-relay` exists for amd64 and
arm64. Until the first release there is no `:latest`; set `:edge` in
`compose.yaml`, or build the image yourself in the cloned repository:
`docker build -f crates/relay/Dockerfile -t ghcr.io/irgendwer92/lebenszeichen-relay:latest lebenszeichen`.

Set your ntfy address in `ntfy/server.yml` (`base-url`), then:

```sh
docker compose up -d
docker compose ps                         # both "running"
curl -i http://127.0.0.1:8080/v1/health   # 200 OK from the relay
```

### ntfy: who may do what

ntfy starts with `deny-all`: nobody may read or write. Grant exactly two
things: everyone may write to topics starting with `up` (the relay sends
wake signals there), and one account for the phones may read them.

```sh
docker compose exec ntfy ntfy user add phone        # asks for a password
docker compose exec ntfy ntfy access everyone 'up*' write-only
docker compose exec ntfy ntfy access phone 'up*' read-only
docker compose exec ntfy ntfy access                # shows the rules
```

`ntfy user add` asks for the password on the terminal; without one (a
script, `ssh` without `-t`) pass it as
`docker compose exec -e NTFY_PASSWORD=… ntfy ntfy user add phone`. The last
command should list `phone` with read-only access to `up*` and `everyone`
with write-only access to `up*`.

All phones of all people on this relay can share the account `phone`; the
topics are random and only the relay learns them. Give the password only to
people you set up.

## 5. Reverse proxy

Pick one. Each example turns off access logs for both names, so that the
server keeps no record of who connected when. Lebenszeichen does not need
them, and the relay itself logs no requests.

**Caddy** on the host: put the two blocks of `proxy/Caddyfile` into your
Caddyfile (for a Debian package: `/etc/caddy/Caddyfile`), replace the names,
`systemctl reload caddy`. Caddy gets the certificates itself and writes no
access log unless a `log` directive is set.

**nginx** on the host: get certificates, for example with
`certbot certonly --nginx -d relay.example.org -d ntfy.example.org`, then
copy `proxy/nginx.conf` to `/etc/nginx/conf.d/lebenszeichen.conf`, replace
the names and certificate paths, `nginx -t && systemctl reload nginx`. The
file passes on the WebSocket of the live connection (`/v1/stream`) and keeps
ntfy's long requests unbuffered.

**Traefik** (v3, in Docker): adapt the names, network, entry point and
certificate resolver in `proxy/compose.traefik.yaml`, then start with both
files instead of step 4's command:

```sh
docker compose -f compose.yaml -f proxy/compose.traefik.yaml up -d
```

The services then have no port on the host; Traefik reaches them through its
network. Each router turns its access log off.

Check from another machine:

```sh
curl -i https://relay.example.org/v1/health   # 200
curl -i https://ntfy.example.org/v1/health    # contains "healthy":true
```

## 6. Get the apps

- **Android**: the Lebenszeichen APK, and **ntfy** (from F-Droid or Google
  Play) as the UnifiedPush distributor.
- **Linux, Windows**: the desktop app. It keeps its own connection while it
  runs and needs no ntfy.
- **Command line, servers**: `lz` (see the README).

No release is published yet. Until then, build them from source as the
README describes; `lz` with `cargo build --release -p lz-cli` (then
`target/release/lz`).

On the phone, set up ntfy once before Lebenszeichen:

1. ntfy → Settings → Manage users → add user: server
   `https://ntfy.example.org`, user `phone`, the password from step 4.
2. ntfy → Settings → Default server: `https://ntfy.example.org`. New
   UnifiedPush registrations use this server.
3. Exempt ntfy from battery optimization (Android settings → Apps → ntfy →
   Battery → Unrestricted); otherwise wake signals arrive late while the
   phone is idle.

## 7. Set up your first device

Open Lebenszeichen → **New account**: relay address
`https://relay.example.org` and the access token from `relay.env`, then
**Create device**.

On a phone, ⚙ → This device lists what Android needs so that requests reach
you while the app is closed: the wake signal ("Wake signal: registered with
ntfy"), notifications, the channel "Requests", full screen and battery
optimization. Each line has a button to the setting.

With the command line instead:

```sh
lz init --relay https://relay.example.org --access-token <token>
```

**More devices of your own** (a laptop next to the phone): on the new device
choose **Set up as another device**; it shows a device code. On the existing
device: Devices → **Add device**, scan or enter the code, and compare the two
check codes on both screens. Show the device code only, never send it: until
the check codes are confirmed, whoever has it can get in the way. A device with the access token can also show a
**setup code** (Devices → Show setup code) for a second account of yours on
this relay; show it only in person, never send it.

## 8. Pair with a second person

The second person needs no access token and no relay address: your
invitation carries a voucher for a mailbox on your relay.

1. You: **+ Invite**, a name for them (only you see it), tick **With
   voucher**, **Create invitation**. The code is valid for 10 minutes and
   once.
2. They: set up their app with **I have an invitation**, then scan the QR
   code (or paste the text code) and accept. Their app waits for the check
   code.
3. Both apps show a six-digit check code. **Compare it in person or on the
   phone**, not over the channel you passed the invitation on. If it matches,
   confirm on your side (**Check code matches: confirm**); if not, reject.

Command line, in this order:

```sh
lz invite --voucher Bert     # Anna; pass on the whole LZ2-… line
lz init                      # Bert, once: no relay address or token needed
lz accept Anna LZ2-…         # Bert
lz sync                      # Anna: answers the acceptance, shows the code
lz sync                      # Bert: shows the same code
lz confirm Bert              # Anna, if the codes match
lz sync                      # Bert: paired
```

## 9. Check that it works

- Request a sign of life from the other person and answer it; then the other
  way round.
  Command line: `lz request Bert` (Anna); `lz sync` and `lz respond Anna`
  (Bert); `lz sync` (Anna); `lz contacts` shows the answer. The default rules
  allow one request per direction every 6 hours.
- Close the app on the phone (swipe it away), lock the screen and send it a
  request. It should ring within a few seconds. If it does not, see
  section 12.

## 10. Operation

**What is stored.** The relay's database (`relay-data` volume) holds mailbox
identifiers, readers' tokens, push addresses and messages waiting for pickup
(deleted on pickup or after 24 hours). ntfy keeps wake signals for 12 hours
and its users in `ntfy-data`. Neither can read requests, answers or names;
both see when devices connect, and the relay sees which mailboxes are active
at the same time, from which one could guess who is paired with whom.

**Backup.** Stop the relay briefly and copy its volume:

```sh
docker compose stop relay
docker run --rm -v lebenszeichen_relay-data:/d -v "$PWD":/b alpine \
  sh -c 'cp /d/relay.sqlite* /b/'   # into a dated directory of your choice
docker compose start relay
```

Losing the relay's database is not a disaster: the devices keep their keys,
but mailboxes are gone, so people have to pair again.

**Update.** Read the release notes, then:

```sh
docker compose pull && docker compose up -d
```

The relay migrates its database at start. Take a backup first.

**Logs.** `docker compose logs relay` shows only start, stop and errors.

## 11. A lost or stolen device

Every device of a person holds the keys of all their contacts. Removing it
(Devices → the device → Remove) stops the relay from waking it and unlinks
your devices, so that it cannot follow along, but a device that is in
someone else's hands could still read and send on the existing contact
mailboxes. So:

1. Remove the lost device on one of your other devices. Your other devices
   are then no longer linked; they keep working, each on its own.
2. End each contact and pair again (section 8), or ask your contacts to do so.
   New mailboxes are unknown to the lost device.
3. Link your other devices again: on each, remove the app's data (Android:
   Settings → Apps → Lebenszeichen → Storage → Clear storage; desktop:
   delete its `device.json`, shown under ⚙) and set it up again as another
   device (Devices → Add device on the one you keep). Its contacts come
   back with the handover.
4. If the lost device had the access token, it can still create mailboxes
   on your relay. Replace the token in `relay.env` (the `sed` line of section 3), then
   `docker compose up -d`; existing mailboxes keep working. Give the new
   token to your remaining devices: Devices → **Change access token** in the
   app (the token, or a setup code with it), or
   `lz access-token <new token>`.

## 12. Troubleshooting

| Symptom | Likely cause |
|---|---|
| "certificate unknown" or another TLS error in the app | the proxy serves no valid certificate for the name; check with `curl -v https://relay.example.org/v1/health` |
| "The relay refuses the push address" | ntfy's `base-url` is not a public https address, or `LZ_PUSH_ALLOWED_HOSTS` does not list it |
| ⚙ → This device: "no UnifiedPush app found" | install ntfy and open it once |
| Requests arrive only when the app is opened | ntfy not exempt from battery optimization; ntfy not logged in for `https://ntfy.example.org` (step 6); ntfy not reachable from the mobile network (try from outside your Wi-Fi); NAT loopback (section 1) |
| "Warning: the clock of this device is about … min ahead of / behind the relay's" | the device's clock is wrong; from 10 minutes of difference messages are refused. Turn on automatic time. |
| `unknown_mailbox` | the other side ended the contact, or the relay's database was lost |
| The live connection drops every minute | the proxy closes idle WebSockets; raise its read timeout (nginx: `proxy_read_timeout`) |
