# Image of the relay. Build from the repository root:
#
#   docker build -f crates/relay/Dockerfile -t lz-relay .
#
# Run (TLS is terminated by a reverse proxy in front):
#
#   docker run -e LZ_ACCESS_TOKEN=... -v lz-relay-data:/data -p 8080:8080 lz-relay

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p lz-relay \
    && mkdir /data

# Distroless carries glibc and the CA certificates needed for wake signals,
# but no shell. The user "nonroot" has the id 65532.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/lz-relay /usr/local/bin/lz-relay
COPY --from=build --chown=65532:65532 /data /data
USER 65532:65532
VOLUME /data
EXPOSE 8080
ENV LZ_LISTEN=0.0.0.0:8080 \
    LZ_DB=/data/relay.sqlite
ENTRYPOINT ["/usr/local/bin/lz-relay"]
