#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Honor CARGO_TARGET_DIR so the build output and the install source agree even
# when the user (or CI) redirects Cargo's target directory.
TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
PREFIX="$HOME/.local/bin"
DB_PATH=""
DO_CONFIG=1
DO_MCP=1
ASSUME_YES=0
ACTION="install"
PURGE=0
DRY_RUN=0

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Echo a command, then run it unless in dry-run mode.
run() {
  printf '    %s\n' "$*"
  if [ "$DRY_RUN" -eq 0 ]; then
    "$@"
  fi
}

# Quote a string so it is safe to copy/paste into a shell (handles spaces/quotes).
shq() { printf '%q' "$1"; }

config_dir() { printf '%s/rusty-rss' "${XDG_CONFIG_HOME:-$HOME/.config}"; }
env_file()   { printf '%s/env' "$(config_dir)"; }
default_db_path() {
  printf '%s/rusty-rss/rusty-rss.sqlite3' "${XDG_DATA_HOME:-$HOME/.local/share}"
}

readonly BINARIES=(rusty-rss rusty-rss-mcp)

build_workspace() {
  command -v cargo >/dev/null 2>&1 \
    || die "cargo not found. Install Rust from https://rustup.rs and re-run."
  log "Building release binaries"
  run cargo build --release --workspace --target-dir "$TARGET_DIR" --manifest-path "$SCRIPT_DIR/Cargo.toml"
}

path_has_dir() {
  case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

install_binaries() {
  log "Installing binaries to $PREFIX"
  run mkdir -p "$PREFIX"
  local bin
  for bin in "${BINARIES[@]}"; do
    run install -m 755 "$TARGET_DIR/release/$bin" "$PREFIX/$bin"
  done
  if ! path_has_dir "$PREFIX"; then
    warn "$PREFIX is not on your PATH. Add this to your shell rc:"
    printf "    export PATH=\"%s:\$PATH\"\n" "$PREFIX"
  fi
}

# Reduce a URL to scheme://host/path, dropping userinfo, query, and fragment.
redact_url() {
  # shellcheck disable=SC2001
  printf '%s' "$1" | sed -E 's#^([a-zA-Z]+://)([^@/?#]*@)?([^/?#]*)(/[^?#]*)?.*#\1\3\4#'
}

# Prompt "question [y/N]"; return 0 for yes. Auto-yes when ASSUME_YES=1.
confirm() {
  if [ "$ASSUME_YES" -eq 1 ]; then return 0; fi
  local reply
  printf '%s [y/N] ' "$1" >&2
  read -r reply
  case "$reply" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

write_config() {
  local cfg_dir env_path feed_url
  cfg_dir="$(config_dir)"
  env_path="$(env_file)"
  log "Configuring $env_path"

  if [ -f "$env_path" ]; then
    local existing
    existing="$(sed -n "s/^RUSTY_RSS_FEED_URL='\(.*\)'\$/\1/p" "$env_path" | head -1)"
    if [ -n "$existing" ]; then
      log "Existing feed URL: $(redact_url "$existing")"
    fi
    confirm "Config exists. Replace it?" || { log "Keeping existing config."; return 0; }
  fi

  if [ -n "${RUSTY_RSS_FEED_URL:-}" ]; then
    feed_url="$RUSTY_RSS_FEED_URL"
  elif [ "$ASSUME_YES" -eq 1 ]; then
    die "--yes requires RUSTY_RSS_FEED_URL to be set in the environment."
  else
    printf 'Reddit saved-items feed URL (input hidden): ' >&2
    read -rs feed_url
    printf '\n' >&2
  fi
  case "$feed_url" in
    http://*|https://*) ;;
    *) die "feed URL must start with http:// or https://" ;;
  esac
  case "$feed_url" in
    *[[:space:]]*) die "feed URL must not contain whitespace" ;;
  esac

  if [ "$DRY_RUN" -eq 1 ]; then
    printf '    write %s (RUSTY_RSS_FEED_URL=%s, RUSTY_RSS_DB_PATH=%s)\n' \
      "$(shq "$env_path")" "$(redact_url "$feed_url")" "$(shq "$DB_PATH")"
    return 0
  fi

  mkdir -p "$cfg_dir"; chmod 700 "$cfg_dir"
  mkdir -p "$(dirname "$DB_PATH")"
  # Escape embedded single quotes as '\'' so the values are safe inside the
  # single-quoted env-file lines below. Unquoted on the assignment RHS so the
  # replacement's backslashes are processed correctly (double quotes would not).
  local esc_feed=${feed_url//\'/\'\\\'\'}
  local esc_db=${DB_PATH//\'/\'\\\'\'}
  (
    umask 077
    cat > "$env_path" <<EOF
# rusty-rss configuration. Loaded with:
#   set -a; source $env_path; set +a
RUSTY_RSS_FEED_URL='$esc_feed'
RUSTY_RSS_DB_PATH='$esc_db'
EOF
  )
  chmod 600 "$env_path"
  log "Wrote $env_path (mode 600). Load it with:"
  printf '    set -a; source %s; set +a\n' "$(shq "$env_path")"
}

register_mcp() {
  local mcp_bin="$PREFIX/rusty-rss-mcp"
  if ! command -v claude >/dev/null 2>&1; then
    log "Claude Code CLI not found. To register the MCP server later, run:"
    printf '    claude mcp add rusty-rss -- %s --db-path %s\n' "$(shq "$mcp_bin")" "$(shq "$DB_PATH")"
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    log "Would register MCP server with Claude Code:"
    printf '    claude mcp add rusty-rss -- %s --db-path %s\n' "$(shq "$mcp_bin")" "$(shq "$DB_PATH")"
    return 0
  fi
  if claude mcp get rusty-rss >/dev/null 2>&1; then
    confirm "MCP server 'rusty-rss' already registered. Re-add?" || {
      log "Leaving existing MCP registration."
      return 0
    }
    run claude mcp remove rusty-rss || true
  fi
  log "Registering MCP server with Claude Code"
  if ! run claude mcp add rusty-rss -- "$mcp_bin" --db-path "$DB_PATH"; then
    warn "claude mcp add failed; register manually with:"
    printf '    claude mcp add rusty-rss -- %s --db-path %s\n' "$(shq "$mcp_bin")" "$(shq "$DB_PATH")"
  fi
}

do_uninstall() {
  log "Removing binaries from $PREFIX"
  local bin
  for bin in "${BINARIES[@]}"; do
    [ -e "$PREFIX/$bin" ] && run rm -f "$PREFIX/$bin"
  done
  if command -v claude >/dev/null 2>&1 && claude mcp get rusty-rss >/dev/null 2>&1; then
    run claude mcp remove rusty-rss || true
  fi
  if [ "$PURGE" -eq 1 ]; then
    log "Purging config and database"
    # Prefer the DB path actually recorded in the config file, so a database
    # installed under a custom --db-path is removed rather than silently left
    # behind. Read it before the config dir is deleted.
    local db_to_remove env_path cfg_db
    db_to_remove="$DB_PATH"
    env_path="$(env_file)"
    if [ -f "$env_path" ]; then
      cfg_db="$(sed -n "s/^RUSTY_RSS_DB_PATH='\(.*\)'\$/\1/p" "$env_path" | head -1)"
      [ -n "$cfg_db" ] && db_to_remove="$cfg_db"
    fi
    run rm -f "$db_to_remove"
    run rm -rf "$(config_dir)"
  else
    log "Left config dir $(config_dir) and database in place (use --purge to remove)."
  fi
}

usage() {
  cat <<'EOF'
install.sh - build and install rusty-rss

Usage: ./install.sh [options]

Options:
  --prefix DIR     Install binaries into DIR (default: ~/.local/bin)
  --db-path PATH   SQLite DB path written to config and passed to MCP
                   (default: ~/.local/share/rusty-rss/rusty-rss.sqlite3)
  --no-config      Skip writing the config env file
  --no-mcp         Skip registering the MCP server with Claude Code
  -y, --yes        Non-interactive; requires RUSTY_RSS_FEED_URL in the env
  --uninstall      Remove binaries and the Claude Code MCP registration
  --purge          With --uninstall, also remove the config dir and DB
  --dry-run        Print actions without executing them
  -h, --help       Show this help
EOF
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --prefix)   PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
      --db-path)  DB_PATH="${2:?--db-path needs a value}"; shift 2 ;;
      --no-config) DO_CONFIG=0; shift ;;
      --no-mcp)   DO_MCP=0; shift ;;
      -y|--yes)   ASSUME_YES=1; shift ;;
      --uninstall) ACTION="uninstall"; shift ;;
      --purge)    PURGE=1; shift ;;
      --dry-run)  DRY_RUN=1; shift ;;
      -h|--help)  usage; exit 0 ;;
      *)          usage; die "unknown argument: $1" ;;
    esac
  done
  [ -n "$DB_PATH" ] || DB_PATH="$(default_db_path)"
  if [ "$PURGE" -eq 1 ] && [ "$ACTION" != "uninstall" ]; then
    die "--purge is only valid with --uninstall"
  fi
}

main() {
  parse_args "$@"
  log "rusty-rss installer (action: $ACTION, dry-run: $DRY_RUN)"
  if [ "$ACTION" = "uninstall" ]; then
    do_uninstall
    return 0
  fi
  build_workspace
  install_binaries
  if [ "$DO_CONFIG" -eq 1 ]; then write_config; fi
  if [ "$DO_MCP" -eq 1 ]; then register_mcp; fi
}

main "$@"
