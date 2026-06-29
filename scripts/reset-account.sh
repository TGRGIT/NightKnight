#!/usr/bin/env bash
# Reset a NightKnight account to nil: no configurations, no data.
#
# Wipes every per-user row across all backends so you can test one input format
# (Dexcom Share, LibreLinkUp, Nightscout, LibreView CSV, Dexcom Clarity CSV, …)
# against a pristine account, reset, and test the next.
#
# The tables cleared mirror service/crates/nightknight-storage/src/sql.rs
# (schema_statements + Collection::all):
#   config: connector_credentials, device_tokens, push_tokens
#   data:   entries, treatments, devicestatus, profile, food, settings
#   account: users  (dropped unless --keep-user)
#
# There is no server-side bulk-delete endpoint, so this talks to the database
# directly. Pick the backend your server / worker is using.
#
# Usage:
#   scripts/reset-account.sh --target <backend> [--user <subject> | --all-users] [options]
#
# Backends (--target):
#   sqlite      file-backed SQLite  (requires --db <path>, or NK_DATABASE_URL=sqlite://…)
#   postgres    Postgres            (uses --db / NK_DATABASE_URL / PG* env; or --compose)
#   d1-local    Cloudflare D1, miniflare local store (wrangler dev)
#   d1-remote   Cloudflare D1, the LIVE deployed database  (production — confirms first)
#
# Scope:
#   --user <subject>   account to wipe, matched on users.subject (default: demo@nightknight)
#   --all-users        wipe every account in the database
#
# Options:
#   --keep-user        keep the users row (just blank its config); default drops it so the
#                      account is recreated fresh on next sign-in
#   --db <url|path>    database URL/path (sqlite file, or postgres:// URL)
#   --compose          run psql inside `deploy/docker compose` (postgres target)
#   --dry-run          print the row counts that WOULD be deleted, then stop
#   --yes              skip the confirmation prompt
#   -h, --help         this help
#
# Examples:
#   # Local dev server backed by a SQLite file, default demo user:
#   scripts/reset-account.sh --target sqlite --db /tmp/nk.db
#
#   # Postgres container, see what's there first:
#   scripts/reset-account.sh --target postgres --compose --user you@example.com --dry-run
#
#   # The live D1 worker, one account, no prompt:
#   scripts/reset-account.sh --target d1-remote --user fergcoon@gmail.com --yes
set -euo pipefail

# --- paths -------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKER_DIR="$REPO_ROOT/service/crates/nightknight-worker"
DEPLOY_DIR="$REPO_ROOT/deploy"
D1_DB_NAME="nightknight"
WRANGLER="${WRANGLER:-npx wrangler@4}"

# --- the schema (keep in sync with sql.rs) -----------------------------------
CONFIG_TABLES=(connector_credentials device_tokens push_tokens)
DATA_TABLES=(entries treatments devicestatus profile food settings)
PER_USER_TABLES=("${CONFIG_TABLES[@]}" "${DATA_TABLES[@]}")

# --- defaults / args ---------------------------------------------------------
TARGET=""
USER_SUBJECT="demo@nightknight"
ALL_USERS=0
KEEP_USER=0
DB="${NK_DATABASE_URL:-}"
COMPOSE=0
DRY_RUN=0
ASSUME_YES=0

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; s/^#$//; /^set -euo/d'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --target)     TARGET="${2:-}"; shift 2 ;;
    --user)       USER_SUBJECT="${2:-}"; ALL_USERS=0; shift 2 ;;
    --all-users)  ALL_USERS=1; shift ;;
    --keep-user)  KEEP_USER=1; shift ;;
    --db)         DB="${2:-}"; shift 2 ;;
    --compose)    COMPOSE=1; shift ;;
    --dry-run)    DRY_RUN=1; shift ;;
    --yes|-y)     ASSUME_YES=1; shift ;;
    -h|--help)    usage; exit 0 ;;
    *)            die "unknown argument: $1 (try --help)" ;;
  esac
done

case "$TARGET" in
  sqlite|postgres|d1-local|d1-remote) ;;
  "") die "missing --target (one of: sqlite, postgres, d1-local, d1-remote)" ;;
  *)  die "unknown --target '$TARGET' (sqlite, postgres, d1-local, d1-remote)" ;;
esac

# Strip a sqlite:// scheme and any ?query for the sqlite CLI.
sqlite_path() {
  local p="$1"
  p="${p#sqlite://}"; p="${p#sqlite:}"
  printf '%s' "${p%%\?*}"
}

# SQL-escape a value for single-quoting (double any single quotes).
sql_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"; }

# WHERE clause selecting the in-scope users for a per-user table (column user_id),
# and for the users table itself (column subject).
if [ "$ALL_USERS" -eq 1 ]; then
  SCOPE_DESC="ALL accounts"
  WHERE_PERUSER="1=1"
  WHERE_USERS="1=1"
else
  [ -n "$USER_SUBJECT" ] || die "--user given an empty subject"
  SCOPE_DESC="account '$USER_SUBJECT'"
  Q_SUBJECT="$(sql_quote "$USER_SUBJECT")"
  WHERE_PERUSER="user_id IN (SELECT id FROM users WHERE subject = $Q_SUBJECT)"
  WHERE_USERS="subject = $Q_SUBJECT"
fi

# --- build the SQL -----------------------------------------------------------
build_count_sql() {
  local first=1 t
  for t in "${PER_USER_TABLES[@]}"; do
    [ $first -eq 1 ] && first=0 || printf ' UNION ALL '
    printf "SELECT '%s' AS tbl, COUNT(*) AS n FROM %s WHERE %s" "$t" "$t" "$WHERE_PERUSER"
  done
  printf " UNION ALL SELECT '%s', COUNT(*) FROM %s WHERE %s;" "users" "users" "$WHERE_USERS"
}

build_delete_sql() {
  local t
  for t in "${PER_USER_TABLES[@]}"; do
    printf 'DELETE FROM %s WHERE %s;\n' "$t" "$WHERE_PERUSER"
  done
  if [ "$KEEP_USER" -eq 1 ]; then
    # Keep the account row but blank its configuration columns.
    printf "UPDATE users SET display_name = NULL, preferred_unit = 'mg/dl', is_admin = 0 WHERE %s;\n" "$WHERE_USERS"
  else
    printf 'DELETE FROM users WHERE %s;\n' "$WHERE_USERS"
  fi
}

# --- backend runners ---------------------------------------------------------
# Each runs a multi-statement SQL file and prints output.
run_sql_file() {
  local file="$1"
  case "$TARGET" in
    sqlite)
      local path; path="$(sqlite_path "$DB")"
      [ -n "$path" ] || die "sqlite target needs --db <file> (or NK_DATABASE_URL=sqlite://file)"
      [ -f "$path" ] || die "sqlite database not found: $path"
      command -v sqlite3 >/dev/null || die "sqlite3 not installed"
      sqlite3 "$path" < "$file"
      ;;
    postgres)
      if [ "$COMPOSE" -eq 1 ]; then
        ( cd "$DEPLOY_DIR" && docker compose exec -T db \
            psql -v ON_ERROR_STOP=1 -U nightknight -d nightknight ) < "$file"
      else
        [ -n "$DB" ] || die "postgres target needs --db postgres://… (or NK_DATABASE_URL), or --compose"
        command -v psql >/dev/null || die "psql not installed"
        psql -v ON_ERROR_STOP=1 "$DB" -f "$file"
      fi
      ;;
    d1-local|d1-remote)
      local flag="--local"; [ "$TARGET" = "d1-remote" ] && flag="--remote"
      ( cd "$WORKER_DIR" && $WRANGLER d1 execute "$D1_DB_NAME" "$flag" --yes --file "$file" )
      ;;
  esac
}

# --- confirmation ------------------------------------------------------------
confirm() {
  [ "$ASSUME_YES" -eq 1 ] && return 0
  local prompt="$1"
  printf '%s\n' "$prompt"
  printf 'Type "yes" to proceed: '
  local reply; read -r reply
  [ "$reply" = "yes" ] || die "aborted"
}

# --- go ----------------------------------------------------------------------
tmp_sql="$(mktemp "${TMPDIR:-/tmp}/nk-reset.XXXXXX.sql")"
trap 'rm -f "$tmp_sql"' EXIT

action="reset to nil"; [ "$KEEP_USER" -eq 1 ] && action="reset to nil (account row kept)"
printf '== NightKnight account reset ==\n'
printf 'target : %s%s\n' "$TARGET" "$([ "$TARGET" = postgres ] && [ "$COMPOSE" -eq 1 ] && printf ' (docker compose)')"
printf 'scope  : %s\n' "$SCOPE_DESC"
printf 'action : %s\n\n' "$action"

# Show current counts first (always — cheap and reassuring).
printf 'Current row counts:\n'
build_count_sql > "$tmp_sql"
run_sql_file "$tmp_sql" || die "failed to read row counts — check connection/target"
printf '\n'

if [ "$DRY_RUN" -eq 1 ]; then
  printf 'Dry run: nothing deleted.\nWould execute:\n'
  build_delete_sql | sed 's/^/  /'
  exit 0
fi

if [ "$TARGET" = "d1-remote" ]; then
  confirm "!! This wipes the LIVE production D1 database for ${SCOPE_DESC}."
elif [ "$ALL_USERS" -eq 1 ]; then
  confirm "This wipes ALL accounts on target '$TARGET'."
fi

build_delete_sql > "$tmp_sql"
run_sql_file "$tmp_sql"

printf '\nDone. Verifying (should all be 0):\n'
build_count_sql > "$tmp_sql"
run_sql_file "$tmp_sql"
