# coolify-rs server image.
#   docker build -t coolify-rs:dev .
# Pinned toolchain matches rust-toolchain.toml; runtime runs as non-root.
FROM rust:1.95.0-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p coolify-rs \
 && cp target/release/coolify-rs /coolify-rs

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home coolify-rs
COPY --from=build /coolify-rs /usr/local/bin/coolify-rs
USER 10001
ENV COOLIFY_RS_LISTEN=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --retries=3 CMD ["/usr/local/bin/coolify-rs", "--version"]
ENTRYPOINT ["/usr/local/bin/coolify-rs"]
CMD ["serve"]
