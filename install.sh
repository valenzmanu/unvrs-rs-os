#!/bin/sh
# UNVRS: one command, from Terminal (not inside a Claude, Codex or T3 Code thread).
#
#   From a clone:      ./install.sh
#   From GitHub:       curl -fsSL https://raw.githubusercontent.com/valenzmanu/unvrs-rs-os/main/install.sh | sh
#
# It keeps a clone at ~/github/unvrs-rs-os (cloned with gh, else git over https, when missing; an
# existing clone is built as it is, uncommitted work untouched; UNVRS_UPDATE=1 pulls with
# --ff-only first when the tree is clean), builds the release binary and runs
# `unvrs setup` with your arguments (--seed <dir>, --no-seed, --trust-hooks, …).
# Safe to re-run. Undo: unvrs uninstall.
#
# Environment: UNVRS_REPO_DIR (default ~/github/unvrs-rs-os), UNVRS_BRANCH (for a new clone,
# default main), UNVRS_UPDATE=1, UNVRS_SETUP_BIN (an already built binary; tests).
set -eu

# Everything runs from main, so a piped `sh` has read the whole script first.
main() {
  REPO_SLUG=valenzmanu/unvrs-rs-os
  BRANCH=${UNVRS_BRANCH:-main}
  say() { printf '==> %s\n' "$*"; }
  die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

  if [ -n "${CODEX_THREAD_ID:-}${CLAUDE_CODE_SESSION_ID:-}${CLAUDECODE:-}" ]; then
    die "run this from Terminal, not inside a Claude or Codex thread: UNVRS refuses captain steps from a model."
  fi

  # From a clone (./install.sh) the repo is this file's directory; piped, it is the default.
  here= piped=1
  case "$0" in
    */install.sh | install.sh) piped= ; here=$(cd "$(dirname "$0")" 2>/dev/null && pwd) || here= ;;
  esac
  if [ -n "$here" ] && [ -f "$here/Cargo.toml" ] && [ -d "$here/unvrs" ]; then
    REPO=$here
  else
    REPO=${UNVRS_REPO_DIR:-$HOME/github/unvrs-rs-os}
  fi

  if [ ! -d "$REPO/.git" ]; then
    [ -e "$REPO" ] && die "$REPO exists but is not a git clone; move it or set UNVRS_REPO_DIR"
    say "Cloning $REPO_SLUG ($BRANCH) into $REPO"
    mkdir -p "$(dirname "$REPO")"
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
      gh repo clone "$REPO_SLUG" "$REPO" -- -q -b "$BRANCH"
    else
      git clone -q -b "$BRANCH" "https://github.com/$REPO_SLUG.git" "$REPO" ||
        die "clone failed; check your network (or clone it yourself into $REPO), then re-run"
    fi
  elif [ "${UNVRS_UPDATE:-}" = 1 ]; then
    if [ -n "$(git -C "$REPO" status --porcelain)" ]; then
      say "Uncommitted changes in $REPO: not pulling; building what is there"
    else
      say "Updating $REPO ($(git -C "$REPO" rev-parse --abbrev-ref HEAD))"
      git -C "$REPO" pull -q --ff-only
    fi
  else
    say "Using $REPO as it is ($(git -C "$REPO" rev-parse --abbrev-ref HEAD) @ $(git -C "$REPO" rev-parse --short HEAD); UNVRS_UPDATE=1 pulls first)"
  fi

  if [ -n "${UNVRS_SETUP_BIN:-}" ]; then
    BIN=$UNVRS_SETUP_BIN
    say "Using the built binary $BIN"
  else
    PATH=$HOME/.cargo/bin:$PATH
    if ! command -v cargo >/dev/null 2>&1; then
      printf '%s\n' "Rust is needed to build UNVRS. Install it with:" \
        "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" \
        "then open a new terminal and run this again." >&2
      exit 1
    fi
    say "Building UNVRS (release; the first build takes a few minutes)"
    (cd "$REPO" && cargo build --release -q -p unvrs)
    BIN=$REPO/target/release/unvrs
  fi

  # Piped from GitHub, stdin is this script: the prompts read the terminal instead.
  if [ -n "$piped" ] && [ ! -t 0 ]; then
    if (: </dev/tty) 2>/dev/null; then exec "$BIN" setup "$@" </dev/tty; fi
    exec "$BIN" setup "$@" </dev/null
  fi
  exec "$BIN" setup "$@"
}

main "$@"
