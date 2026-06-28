#!/usr/bin/env bash
# shellcheck disable=SC2034 # Variables set in parse_args, used in later tasks
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
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

config_dir() { printf '%s/rusty-rss' "${XDG_CONFIG_HOME:-$HOME/.config}"; }
env_file()   { printf '%s/env' "$(config_dir)"; }
default_db_path() {
  printf '%s/rusty-rss/rusty-rss.sqlite3' "${XDG_DATA_HOME:-$HOME/.local/share}"
}

BINARIES=(rusty-rss rusty-rss-mcp)

build_workspace() {
  command -v cargo >/dev/null 2>&1 \
    || die "cargo not found. Install Rust from https://rustup.rs and re-run."
  log "Building release binaries"
  run cargo build --release --bins --manifest-path "$SCRIPT_DIR/Cargo.toml"
}

path_has_dir() {
  case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

install_binaries() {
  log "Installing binaries to $PREFIX"
  run mkdir -p "$PREFIX"
  local bin
  for bin in "${BINARIES[@]}"; do
    run install -m 755 "$SCRIPT_DIR/target/release/$bin" "$PREFIX/$bin"
  done
  if ! path_has_dir "$PREFIX"; then
    warn "$PREFIX is not on your PATH. Add this to your shell rc:"
    printf "    export PATH=\"%s:\$PATH\"\n" "$PREFIX"
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
}

main() {
  parse_args "$@"
  log "rusty-rss installer (action: $ACTION, dry-run: $DRY_RUN)"
  if [ "$ACTION" = "uninstall" ]; then
    log "uninstall not yet implemented"
    return 0
  fi
  build_workspace
  install_binaries
}

main "$@"
