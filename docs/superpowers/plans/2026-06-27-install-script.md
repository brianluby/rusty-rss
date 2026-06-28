# rusty-rss `install.sh` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a single idempotent `install.sh` that builds the workspace, installs both binaries to `~/.local/bin`, scaffolds a secured config env file, and registers the MCP server with Claude Code.

**Architecture:** One bash script at the repo root, `set -euo pipefail`, structured as small functions (one per phase) dispatched from a `main` that parses flags first. Each phase is independently skippable and re-runnable. Verification is `shellcheck` plus real `--dry-run` and live smoke runs (a bash installer has no unit-test harness in this repo, so `shellcheck` + smoke runs are the test cycle).

**Tech Stack:** bash, shellcheck, cargo (release build), the `claude` CLI (optional, for MCP registration).

## Global Constraints

- Script lives at repo root: `install.sh`, executable (`chmod +x`), `#!/usr/bin/env bash` shebang.
- `set -euo pipefail` at the top.
- Must pass `shellcheck install.sh` with no warnings.
- Binary install default dir: `~/.local/bin`. Override: `--prefix DIR`.
- Config dir `~/.config/rusty-rss/` (`chmod 700`); env file `~/.config/rusty-rss/env` (`chmod 600`).
- Default DB path: `~/.local/share/rusty-rss/rusty-rss.sqlite3`. Override: `--db-path PATH`.
- Feed token read with hidden input (`read -rs`); never echoed, never placed in any command's argv.
- Env var names exactly: `RUSTY_RSS_FEED_URL`, `RUSTY_RSS_DB_PATH`.
- MCP server name registered as exactly `rusty-rss`; command `rusty-rss-mcp --db-path <resolved db>`.
- MCP registration failure must never abort the install.
- All paths use `$HOME` / expansion that respects `XDG_CONFIG_HOME` and `XDG_DATA_HOME` when set, falling back to `~/.config` and `~/.local/share`.

---

### Task 1: Script skeleton, flag parsing, help, dry-run plumbing

**Files:**
- Create: `install.sh`

**Interfaces:**
- Consumes: nothing.
- Produces: global vars set by flag parsing — `PREFIX`, `DB_PATH`, `DO_CONFIG` (0/1), `DO_MCP` (0/1), `ASSUME_YES` (0/1), `ACTION` (`install`|`uninstall`), `PURGE` (0/1), `DRY_RUN` (0/1). Helper functions `run()` (echoes + executes, or only echoes under dry-run), `log()`, `warn()`, `die()`, `usage()`. Path helpers `config_dir()`, `env_file()`, `default_db_path()`.

- [ ] **Step 1: Write the skeleton**

```bash
#!/usr/bin/env bash
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
}

main "$@"
```

- [ ] **Step 2: Make it executable and verify help + dry-run**

Run:
```bash
chmod +x install.sh
./install.sh --help
./install.sh --dry-run --no-config --no-mcp
./install.sh --bogus; echo "exit=$?"
```
Expected: help text prints and exits 0; dry-run prints the banner; `--bogus` prints usage + `error: unknown argument: --bogus` and exits non-zero.

- [ ] **Step 3: Lint**

Run: `shellcheck install.sh`
Expected: no output (clean).

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat: install.sh skeleton with flag parsing and dry-run"
```

---

### Task 2: Build phase + binary install

**Files:**
- Modify: `install.sh`

**Interfaces:**
- Consumes: `PREFIX`, `DRY_RUN`, `SCRIPT_DIR`, `run`, `log`, `warn`, `die`.
- Produces: `build_workspace()` (cargo release build), `install_binaries()` (copies `rusty-rss` and `rusty-rss-mcp` into `$PREFIX`, warns if `$PREFIX` not on PATH). Global readonly `BINARIES=(rusty-rss rusty-rss-mcp)`.

- [ ] **Step 1: Add build + install functions**

Insert after the path helpers:

```bash
BINARIES=(rusty-rss rusty-rss-mcp)

build_workspace() {
  command -v cargo >/dev/null 2>&1 \
    || die "cargo not found. Install Rust from https://rustup.rs and re-run."
  log "Building release binaries"
  run cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"
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
    printf '    export PATH="%s:$PATH"\n' "$PREFIX"
  fi
}
```

Wire them into `main` before the existing final `log`, guarded by action:

```bash
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
```

- [ ] **Step 2: Dry-run shows the plan**

Run: `./install.sh --dry-run --no-config --no-mcp`
Expected: prints `cargo build --release ...`, `mkdir -p ...`, two `install -m 755 ...` lines. No files created.

- [ ] **Step 3: Real run installs binaries**

Run:
```bash
./install.sh --no-config --no-mcp
ls -l "$HOME/.local/bin/rusty-rss" "$HOME/.local/bin/rusty-rss-mcp"
"$HOME/.local/bin/rusty-rss" --help | head -1
```
Expected: both binaries exist with mode `755`; `rusty-rss --help` prints its usage line.

- [ ] **Step 4: Lint + commit**

```bash
shellcheck install.sh
git add install.sh
git commit -m "feat: build workspace and install binaries"
```

---

### Task 3: Config scaffold phase

**Files:**
- Modify: `install.sh`

**Interfaces:**
- Consumes: `DO_CONFIG`, `DB_PATH`, `ASSUME_YES`, `DRY_RUN`, `config_dir`, `env_file`, `run`, `log`, `warn`, `die`.
- Produces: `redact_url()` (reduce a URL to `scheme://host/path`), `confirm()` (y/N prompt, returns 0/1; auto-yes under `ASSUME_YES`), `write_config()` (interactive feed-URL capture + env file write).

- [ ] **Step 1: Add redact + confirm helpers**

```bash
# Reduce a URL to scheme://host/path, dropping userinfo, query, and fragment.
redact_url() {
  # shellcheck disable=SC2001
  printf '%s' "$1" | sed -E 's#^([a-zA-Z]+://[^/?#]*)(/[^?#]*)?.*#\1\2#'
}

# Prompt "question [y/N]"; return 0 for yes. Auto-yes when ASSUME_YES=1.
confirm() {
  if [ "$ASSUME_YES" -eq 1 ]; then return 0; fi
  local reply
  printf '%s [y/N] ' "$1" >&2
  read -r reply
  case "$reply" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}
```

- [ ] **Step 2: Add write_config**

```bash
write_config() {
  local cfg_dir env_path feed_url
  cfg_dir="$(config_dir)"
  env_path="$(env_file)"
  log "Configuring $env_path"

  if [ -f "$env_path" ]; then
    local existing
    existing="$(sed -n 's/^RUSTY_RSS_FEED_URL=//p' "$env_path" | head -1)"
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

  if [ "$DRY_RUN" -eq 1 ]; then
    printf '    write %s (RUSTY_RSS_FEED_URL=%s, RUSTY_RSS_DB_PATH=%s)\n' \
      "$env_path" "$(redact_url "$feed_url")" "$DB_PATH"
    return 0
  fi

  mkdir -p "$cfg_dir"; chmod 700 "$cfg_dir"
  mkdir -p "$(dirname "$DB_PATH")"
  umask 077
  cat > "$env_path" <<EOF
# rusty-rss configuration. Loaded with:
#   set -a; source $env_path; set +a
RUSTY_RSS_FEED_URL=$feed_url
RUSTY_RSS_DB_PATH=$DB_PATH
EOF
  chmod 600 "$env_path"
  log "Wrote $env_path (mode 600). Load it with:"
  printf '    set -a; source %s; set +a\n' "$env_path"
}
```

Wire into `main` after `install_binaries`:

```bash
  [ "$DO_CONFIG" -eq 1 ] && write_config
```

- [ ] **Step 3: Dry-run redacts the token**

Run: `RUSTY_RSS_FEED_URL='https://old.reddit.com/saved.rss?feed=SECRET&user=ME' ./install.sh --dry-run --no-mcp`
Expected: the `write ...` line shows `https://old.reddit.com/saved.rss` and does NOT contain `SECRET` or the query string.

- [ ] **Step 4: Real run writes a 600 env file**

Run:
```bash
RUSTY_RSS_FEED_URL='https://old.reddit.com/saved.rss?feed=SECRET&user=ME' \
  ./install.sh --no-mcp -y
stat -f '%Sp %N' "$(printf '%s/rusty-rss/env' "${XDG_CONFIG_HOME:-$HOME/.config}")"
```
Expected: file mode is `-rw-------`; file contains both env vars with the full feed URL.

- [ ] **Step 5: Re-run keeps existing config**

Run: `./install.sh --no-mcp --dry-run` and answer `n` at the replace prompt (or confirm the existing-URL line is redacted).
Expected: prints the redacted existing feed URL and "Keeping existing config."

- [ ] **Step 6: Lint + commit**

```bash
shellcheck install.sh
git add install.sh
git commit -m "feat: scaffold secured config env file"
```

---

### Task 4: MCP registration phase

**Files:**
- Modify: `install.sh`

**Interfaces:**
- Consumes: `DO_MCP`, `PREFIX`, `DB_PATH`, `confirm`, `run`, `log`, `warn`.
- Produces: `register_mcp()` (registers `rusty-rss` with Claude Code if the `claude` CLI exists; prints instructions otherwise; never aborts).

- [ ] **Step 1: Add register_mcp**

```bash
register_mcp() {
  local mcp_bin="$PREFIX/rusty-rss-mcp"
  if ! command -v claude >/dev/null 2>&1; then
    log "Claude Code CLI not found. To register the MCP server later, run:"
    printf '    claude mcp add rusty-rss -- %s --db-path %s\n' "$mcp_bin" "$DB_PATH"
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
    printf '    claude mcp add rusty-rss -- %s --db-path %s\n' "$mcp_bin" "$DB_PATH"
  fi
}
```

Wire into `main` after the config line:

```bash
  [ "$DO_MCP" -eq 1 ] && register_mcp
```

- [ ] **Step 2: Dry-run prints the add command**

Run: `./install.sh --dry-run --no-config`
Expected: prints `claude mcp add rusty-rss -- <prefix>/rusty-rss-mcp --db-path <db>` (or the "CLI not found" instructions if `claude` is absent). No registration occurs.

- [ ] **Step 3: Real run registers (if claude present)**

Run:
```bash
./install.sh --no-config
claude mcp get rusty-rss 2>/dev/null | head -3 || echo "claude CLI absent"
```
Expected: server `rusty-rss` is listed pointing at `rusty-rss-mcp --db-path <db>`, OR the absent-CLI branch printed instructions and exit stayed 0.

- [ ] **Step 4: Lint + commit**

```bash
shellcheck install.sh
git add install.sh
git commit -m "feat: register MCP server with Claude Code"
```

---

### Task 5: Uninstall phase

**Files:**
- Modify: `install.sh`

**Interfaces:**
- Consumes: `PREFIX`, `PURGE`, `config_dir`, `default_db_path`, `DB_PATH`, `BINARIES`, `run`, `log`.
- Produces: `do_uninstall()` (removes binaries, removes MCP registration, optionally purges config + DB).

- [ ] **Step 1: Add do_uninstall**

```bash
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
    run rm -rf "$(config_dir)"
    run rm -f "$DB_PATH"
  else
    log "Left config dir $(config_dir) and database in place (use --purge to remove)."
  fi
}
```

Replace the placeholder uninstall branch in `main`:

```bash
  if [ "$ACTION" = "uninstall" ]; then
    do_uninstall
    return 0
  fi
```

- [ ] **Step 2: Dry-run uninstall**

Run: `./install.sh --uninstall --dry-run`
Expected: prints `rm -f` for any installed binary, a `claude mcp remove rusty-rss` line if registered, and the "Left config dir ... in place" message. Nothing removed.

- [ ] **Step 3: Real uninstall round-trip**

Run:
```bash
./install.sh --no-config --no-mcp        # ensure binaries present
./install.sh --uninstall
ls "$HOME/.local/bin/rusty-rss" 2>&1 || echo "removed"
```
Expected: binaries gone (`removed`), config + DB untouched.

- [ ] **Step 4: Lint + commit**

```bash
shellcheck install.sh
git add install.sh
git commit -m "feat: uninstall and purge support"
```

---

### Task 6: README update

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the finished `install.sh`.
- Produces: documentation only.

- [ ] **Step 1: Add an Install section after the title paragraph**

Insert before the existing `## Quick Start` heading:

```markdown
## Install

```bash
./install.sh
```

This builds release binaries, installs `rusty-rss` and `rusty-rss-mcp` to
`~/.local/bin`, writes a secured config file at `~/.config/rusty-rss/env`
(prompting for your feed URL with hidden input), and registers the MCP server
with Claude Code. Re-running is safe. Useful flags: `--prefix DIR`,
`--db-path PATH`, `--no-config`, `--no-mcp`, `-y`, `--uninstall`, `--dry-run`.

Load your config in a shell before running `sync`:

```bash
set -a; source ~/.config/rusty-rss/env; set +a
rusty-rss sync
```
```

- [ ] **Step 2: Verify rendering**

Run: `sed -n '/## Install/,/## Quick Start/p' README.md`
Expected: the new Install section appears immediately above Quick Start.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document install.sh in README"
```

---

## Self-Review

**Spec coverage:**
- Phase 1 Build → Task 2 (`build_workspace`). ✓
- Phase 2 Install binaries + PATH warning → Task 2 (`install_binaries`). ✓
- Phase 3 Config scaffold (XDG dirs, hidden input, 600/700 perms, redacted existing-secret display, load instructions) → Task 3. ✓
- Phase 4 MCP register (claude present/absent, re-add confirm, never abort) → Task 4. ✓
- Flags `--prefix/--db-path/--no-config/--no-mcp/-y/--uninstall/--purge/--dry-run/-h` → Task 1 parse_args; behaviors realized in Tasks 2–5. ✓
- Security (hidden token, 600 env, no secret to MCP, redacted display) → Tasks 1/3/4. ✓
- Verification (shellcheck, dry-run, smoke, README) → every task lints; README in Task 6. ✓
- Note: the spec's "offer to append the source line to shell rc" was intentionally reduced to printing the load instruction (Task 3 Step 2) — editing a user's rc silently is riskier than the value it adds, and the printed line is copy-pasteable. Flagged here as a conscious scope trim.

**Placeholder scan:** No TBD/TODO; all code blocks are complete; the Task 2 uninstall stub is replaced in Task 5.

**Type consistency:** Function names consistent across tasks (`config_dir`, `env_file`, `default_db_path`, `run`, `confirm`, `redact_url`, `BINARIES`). MCP server name `rusty-rss` and command `rusty-rss-mcp --db-path` identical in Tasks 4 and 5. Env var names match the Rust core (`RUSTY_RSS_FEED_URL`, `RUSTY_RSS_DB_PATH`).
