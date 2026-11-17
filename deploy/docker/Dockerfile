# lz-server (plus the lz command line) as a container image.
# Build context: the repository root.
#   docker build -f deploy/docker/Dockerfile -t lebenszeichen .

FROM rust:1.89-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p lz-server -p lz-cli \
 && install -D -t /out target/release/lz-server target/release/lz

FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="Lebenszeichen" \
      org.opencontainers.image.description="lz-server: local interface, reported states and setup page of Lebenszeichen" \
      org.opencontainers.image.source="https://github.com/irgendwer92/Lebenszeichen" \
      org.opencontainers.image.url="https://github.com/irgendwer92/Lebenszeichen" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

# ca-certificates: the relay certificate is verified by the system store.
# jq: reads /data/options.json in the Home Assistant app.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates jq \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 1000 --user-group --no-create-home --shell /usr/sbin/nologin lebenszeichen \
 && install -d -o 1000 -g 1000 -m 0700 /data /run/lebenszeichen

COPY --from=build /out/lz-server /out/lz /usr/local/bin/
COPY deploy/docker/entrypoint.sh /usr/local/bin/lebenszeichen-entrypoint

ENV LZ_STATE=/data/device.json
VOLUME ["/data"]

# One image for both uses. It runs as root by default because the Home
# Assistant Supervisor starts it with the image's user and mounts /data and
# /share root-owned. Plain Docker should not run as root: set
# `user: "1000:1000"` (compose) or `--user 1000:1000` (docker run), see
# deploy/docker/compose.yaml.

ENTRYPOINT ["lebenszeichen-entrypoint"]
CMD ["--socket", "/run/lebenszeichen/lz.sock"]
