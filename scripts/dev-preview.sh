#!/bin/sh
# Dev/preview launcher: runs the NightKnight server in "dev" auth mode (a fixed
# demo user, no proxy needed) against an in-memory database, serving the web SPA.
# Used by .claude/launch.json for the preview panel and for local poking.
set -e
export NK_AUTH_MODE="${NK_AUTH_MODE:-dev}"
export NK_DEV_USER="${NK_DEV_USER:-demo@nightknight}"
export NK_DATABASE_URL="${NK_DATABASE_URL:-sqlite::memory:}"
export NK_BIND="${NK_BIND:-127.0.0.1:8790}"
export NK_WEB_DIR="${NK_WEB_DIR:-web/dist}"
# Dev-only connector encryption key (hex, 32 bytes) so the connector UI works locally.
export NK_CONNECTOR_KEY="${NK_CONNECTOR_KEY:-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef}"
exec ./target/debug/nightknight-server
