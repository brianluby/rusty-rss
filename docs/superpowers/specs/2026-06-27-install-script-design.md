# rusty-rss `install.sh` — Design

**Date:** 2026-06-27
**Status:** Approved (design)

## Goal

Provide a single, idempotent installer that takes a fresh clone of `rusty-rss`
to a working, agent-ready setup: release binaries on `PATH`, a secured config
file holding the feed token and DB path, and the MCP server registered with
Claude Code.

## Scope

`install.sh` at the repo root, written in bash (`set -euo pipefail`), passing
`shellcheck` cleanly. It runs four phases, each individually skippable, and is
safe to re-run.

### Phase 1 — Build
- Verify `cargo` is on `PATH`; if absent, fail with a clear message pointing to
  https://rustup.rs.
- Run `cargo build --release` for the workspace, producing `rusty-rss` and
  `rusty-rss-mcp` under `target/release/`.

### Phase 2 — Install binaries
- Copy `rusty-rss` and `rusty-rss-mcp` to `~/.local/bin` (override with
  `--prefix DIR`); `chmod 755`. Create the directory if missing.
- If the install dir is not on `PATH`, print the exact `export PATH=...` line for
  the user's shell rc. Do not edit the rc silently.

### Phase 3 — Config scaffold (interactive)
- Config dir: `~/.config/rusty-rss/` (`chmod 700`).
- Env file: `~/.config/rusty-rss/env` (`chmod 600`).
- Default DB path: `~/.local/share/rusty-rss/rusty-rss.sqlite3` (XDG data dir,
  created). Override with `--db-path PATH`. An installed tool must not write into
  `$PWD`.
- Prompt for the feed URL using hidden input (`read -rs`) so the embedded Reddit
  `feed` token never reaches the terminal echo or shell history. Validate that it
  begins with `http://` or `https://` (and rejects whitespace).
- Write `RUSTY_RSS_FEED_URL=...` and `RUSTY_RSS_DB_PATH=...` to the env file.
- If the env file already exists, display its contents with the feed URL reduced
  to `scheme://host/path` (mirroring `redact_feed_url` in the Rust core) and ask
  keep / replace. Never overwrite a stored secret without confirmation.
- Print the load instruction `set -a; source ~/.config/rusty-rss/env; set +a`,
  and offer (confirmation required, default no) to append it to the detected
  shell rc.

### Phase 4 — Register MCP with Claude Code
- If the `claude` CLI is present:
  `claude mcp add rusty-rss -- <abs>/rusty-rss-mcp --db-path <resolved db>`.
  If a `rusty-rss` server is already registered, re-add only on confirmation.
- If `claude` is absent: print the exact command for the user to run later.
- MCP registration failure never aborts the install (the MCP server reads the DB
  only; it does not need the feed token).

## Flags

| Flag | Effect |
|------|--------|
| `--prefix DIR` | Binary install dir (default `~/.local/bin`) |
| `--db-path PATH` | DB path written to config and passed to MCP |
| `--no-config` | Skip Phase 3 |
| `--no-mcp` | Skip Phase 4 |
| `-y`, `--yes` | Non-interactive; requires `RUSTY_RSS_FEED_URL` in env so it never blocks on prompts |
| `--uninstall` | Remove binaries and run `claude mcp remove rusty-rss`; keep env file + DB |
| `--purge` | With `--uninstall`, also remove config dir and DB |
| `--dry-run` | Print planned actions without executing |
| `-h`, `--help` | Usage |

## Security

- Feed token read with hidden input; never echoed, never in argv of any command.
- Env file `chmod 600`, config dir `chmod 700`.
- Existing-secret display is redacted to host+path before showing.
- No secret is passed to `claude mcp add` (MCP needs only `--db-path`).

## Out of scope (YAGNI)

- Claude Desktop config merge.
- Homebrew / `.deb` / other OS packaging.
- LLM (`OPENAI_API_KEY`) config — `sync` works without it; the user can add it to
  the same env file manually.
- System-wide (`/usr/local/bin`, sudo) install — `--prefix` covers it if needed.

## Verification

- `shellcheck install.sh` passes.
- `--dry-run` prints the full plan without side effects.
- Real run: both binaries land in the prefix and `rusty-rss --help` runs;
  env file created with `600` perms; `claude mcp add` succeeds when `claude` is
  present (or instructions printed when absent).
- README "Quick Start" updated to lead with `./install.sh`.
