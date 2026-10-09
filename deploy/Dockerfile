# NightKnight container image (the non-Cloudflare deployment target).
# Multi-stage: build the axum server, then ship a slim runtime with the web SPA.
#
# Base images are pinned by digest for reproducible, reviewable builds; Dependabot
# (.github/dependabot.yml) proposes digest bumps. CI scans the result (container.yml).

FROM rust:1-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6 AS build
WORKDIR /app
COPY . .
# Only the server (and its native deps) are built; the wasm worker is skipped.
RUN cargo build --release -p nightknight-server

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
# Apply pending Debian security updates on top of the pinned base, so a CVE that is
# already fixed upstream doesn't ship just because the base image hasn't been rebuilt.
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
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
