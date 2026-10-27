# coolify-rs server image (API + web UI).
#   docker build -t coolify-rs:dev .
# Pinned toolchains; the runtime runs as non-root.
FROM node:24.15.0-alpine AS ui
WORKDIR /ui
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run check && npm run build

FROM rust:1.95.0-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p coolify-rs \
 && cp target/release/coolify-rs /coolify-rs

FROM debian:bookworm-slim
# openssh-client: remote execution runs the system ssh (ADR 0009).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates openssh-client \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home coolify-rs \
 && install -d -o 10001 -g 10001 -m 0700 /data/coolify-rs
COPY --from=build /coolify-rs /usr/local/bin/coolify-rs
COPY --from=ui /ui/dist /usr/share/coolify-rs/ui
USER 10001
ENV COOLIFY_RS_LISTEN=0.0.0.0:8080 \
    COOLIFY_RS_UI_DIR=/usr/share/coolify-rs/ui \
    COOLIFY_RS_DATA_DIR=/data/coolify-rs
# Pinned SSH host keys; key files are only written for the length of a command.
VOLUME /data/coolify-rs
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/usr/local/bin/coolify-rs", "--version"]
ENTRYPOINT ["/usr/local/bin/coolify-rs"]
CMD ["serve"]
