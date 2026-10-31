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
# openssh-client: remote execution runs the system ssh (ADR 0009). curl: the public IP lookup.
# Uid 9999 owns /data/coolify on the host, as it does for upstream's image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl openssh-client \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 9999 --no-create-home coolify-rs \
 && install -d -o 9999 -g 9999 -m 0700 /data/coolify-rs
COPY --from=build /coolify-rs /usr/local/bin/coolify-rs
COPY --from=ui /ui/dist /usr/share/coolify-rs/ui
COPY templates/service-templates-latest.json /usr/share/coolify-rs/templates/service-templates-latest.json
USER 9999
ENV COOLIFY_RS_LISTEN=0.0.0.0:8080 \
    COOLIFY_RS_UI_DIR=/usr/share/coolify-rs/ui \
    COOLIFY_RS_TEMPLATES_DIR=/usr/share/coolify-rs/templates \
    COOLIFY_RS_DATA_DIR=/data/coolify-rs
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/usr/local/bin/coolify-rs", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/coolify-rs"]
CMD ["serve"]
