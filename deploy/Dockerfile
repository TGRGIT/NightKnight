# NightKnight container image (the non-Cloudflare deployment target).
# Multi-stage: build the axum server, then ship a slim runtime with the web SPA.

FROM rust:1-bookworm AS build
WORKDIR /app
COPY . .
# Only the server (and its native deps) are built; the wasm worker is skipped.
RUN cargo build --release -p nightknight-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/nightknight-server /usr/local/bin/nightknight-server
COPY --from=build /app/web/dist /app/web/dist
ENV NK_WEB_DIR=/app/web/dist \
    NK_BIND=0.0.0.0:8787
EXPOSE 8787
# A non-root user for defence in depth.
RUN useradd -r -u 10001 nightknight
USER nightknight
CMD ["nightknight-server"]
