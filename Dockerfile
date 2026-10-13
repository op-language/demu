# The demu daemon image: a multi-stage build with an isolated runtime.
# The build stage compiles the workspace with cargo cache mounts, and the
# runtime stage carries only the binary and the machine configs.

# The build stage. The rust:1-slim toolchain is Debian trixie based, so
# the runtime stage uses the matching trixie slim image for glibc.
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
# The cache mounts keep the registry archive and the compiled object
# files between builds. The binary copies out because the target
# directory lives in the cache, not in the image layers.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p demu \
    && cp target/release/demu /tmp/demu

# The runtime stage: the daemon binary, the machine configs on the
# XDG config path, and a writable data root for the session output
# directories. The daemon runs unprivileged; the compose stack publishes
# the MCP port and mounts the volume at /data.
FROM debian:trixie-slim
RUN useradd --create-home --shell /usr/sbin/nologin demu
COPY --from=build /tmp/demu /usr/local/bin/demu
COPY configs/ /config/demu/configs/
ENV XDG_CONFIG_HOME=/config \
    XDG_DATA_HOME=/data
RUN mkdir -p /data \
    && chown demu:demu /data \
    && chmod 0755 /config /config/demu /config/demu/configs
USER demu
EXPOSE 6502
ENTRYPOINT ["/usr/local/bin/demu"]
CMD ["daemon", "--http", "0.0.0.0:6502"]