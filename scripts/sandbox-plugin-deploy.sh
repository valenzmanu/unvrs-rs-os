#!/bin/sh
# PID 199 sandbox: a release deploy must bring the deployed slot's plugin (its skills) to
# Claude Code and Codex before doctor runs, and a rollback must bring the old plugin back.
# Reproduces the 2026-10-05 failure ("codex skills: skills/list lacks unvrs:conduct",
# "claude commands: init slash_commands lack /unvrs:conduct") with an old builder, and
# passes with a fixed one. Real Claude Code and Codex CLIs, no model calls (init and
# skills/list only), so no login is needed.
#
# Fresh UNVRS_HOME, CLAUDE_CONFIG_DIR, CODEX_HOME, LaunchAgents dir and launchd label, its
# own Observatory port: the live kernel and the captain's harness configs are untouched.
#
#   scripts/sandbox-plugin-deploy.sh <live-slot> <commit> [<builder>]
# <live-slot>  slot in ~/.unvrs/versions to start from; installed with its own binary
# <commit>     the candidate; its slot must be staged in ~/.unvrs/versions already
# <builder>    binary that runs `deploy` (default: this checkout's target/debug/unvrs)
# SANDBOX_CANDIDATE_BIN=<binary> makes the candidate slot from that binary (iteration
# before staging; the commit is only its label).
# Prints PASS/FAIL lines; exits 0 only when every expectation holds.
set -u
if [ "$#" -lt 2 ]; then
    echo "usage: $0 <live-slot> <commit> [<builder>]" >&2
    exit 2
fi
repo=$(cd "$(dirname "$0")/.." && pwd -P)
live_home="$HOME/.unvrs"
old="$1"
commit=$(git -C "$repo" rev-parse --verify "$2^{commit}") || exit 2
builder="${3:-$repo/target/debug/unvrs}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)
new="$version+g$(printf %.12s "$commit")"
cand_bin="${SANDBOX_CANDIDATE_BIN:-}"
slots="$old"
[ -n "$cand_bin" ] || slots="$old $new"
for s in $slots; do
    [ -f "$live_home/versions/$s/manifest.json" ] || {
        echo "slot $s is not staged in $live_home/versions" >&2
        exit 2
    }
done

S=$(mktemp -d /tmp/upd.XXXX)
S=$(cd "$S" && pwd -P)
label="dev.unvrs.sandbox.$(basename "$S")"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
# A sandbox, not the live kernel: the driven-turn guard is about the live one.
unset UNVRS_DRIVEN_PID UNVRS_SOCKET UNVRS_TOKEN UNVRS_DAEMON CLAUDECODE CLAUDE_CODE_ENTRYPOINT
export UNVRS_HOME="$S/u" CLAUDE_CONFIG_DIR="$S/c" CODEX_HOME="$S/x" \
    UNVRS_LAUNCHAGENTS_DIR="$S/la" UNVRS_LAUNCHD_LABEL="$label" \
    UNVRS_OBSERVATORY_PORT="$port" PATH="$S/path:$PATH"
mkdir -p "$S/u/versions" "$S/u/bin" "$S/c" "$S/x" "$S/la" "$S/path"
bin="$S/u/bin/unvrs"
ln -s "$bin" "$S/path/unvrs"
cleanup() {
    launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1
    "$bin" kernel stop >/dev/null 2>&1
}
trap cleanup EXIT
fail=0
pass() { echo "PASS $*"; }
flunk() {
    echo "FAIL $*"
    fail=1
}
point() { # slot: bin/unvrs and versions/current name it, as on the live home
    ln -sfn "$S/u/versions/$1/unvrs" "$bin"
    printf '%s\n' "$1" >"$S/u/versions/current"
}
claude_cmds() { # the unvrs slash commands Claude Code's init lists, one line
    (cd "$S/u" && claude -p --output-format stream-json --verbose ok 2>/dev/null) |
        python3 -c 'import json, sys
for l in sys.stdin:
    try: v = json.loads(l)
    except ValueError: continue
    if v.get("type") == "system" and v.get("subtype") == "init":
        print(" ".join(sorted(c for c in v.get("slash_commands", []) if c.startswith("unvrs:"))))
        break'
}
codex_skills() { # the unvrs skills in Codex's cache for the installed version
    ls "$S/x/plugins/cache/unvrs-local/unvrs/"*/skills 2>/dev/null | sort -u | tr '\n' ' '
}
trust_codex_hooks() { # what the captain did once on the live home (Codex: /hooks, Trust)
    (cd "$S/u" && python3 - "$S/u" "$S/x/config.toml") <<'EOF'
import json, subprocess, sys
p = subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, text=True)
def send(m):
    p.stdin.write(json.dumps(m) + "\n")
    p.stdin.flush()
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "sandbox", "version": "1"}}})
send({"jsonrpc": "2.0", "method": "initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "hooks/list", "params": {"cwds": [sys.argv[1]]}})
while True:
    v = json.loads(p.stdout.readline())
    if v.get("id") == 2:
        break
p.kill()
hooks = [h for d in v["result"]["data"] for h in d.get("hooks", []) if h.get("pluginId") == "unvrs@unvrs-local"]
with open(sys.argv[2], "a") as f:
    for h in hooks:
        f.write(f'\n[hooks.state."{h["key"]}"]\ntrusted_hash = "{h["currentHash"]}"\n')
print(f"trusted {len(hooks)} codex hooks")
EOF
}
claude_version() {
    python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["plugins"]["unvrs@unvrs-local"][0]["version"])' \
        "$S/c/plugins/installed_plugins.json"
}

echo "sandbox: $S (label $label, observatory port $port)"
echo "start: $old; candidate: $new; builder: $builder"
cp -Rp "$live_home/versions/$old" "$S/u/versions/$old"
if [ -n "$cand_bin" ]; then
    mkdir -p "$S/u/versions/$new"
    cp "$cand_bin" "$S/u/versions/$new/unvrs"
    printf '{"id":"%s","version":"%s","commit":"%s","ref":"%s","branch":null,"tested":false}\n' \
        "$new" "$version" "$commit" "$commit" >"$S/u/versions/$new/manifest.json"
else
    cp -Rp "$live_home/versions/$new" "$S/u/versions/$new"
fi
# 1. The live state before the deploy: the old slot installed, its plugin on both hosts.
point "$old"
"$bin" install >"$S/install.log" 2>&1 || {
    cat "$S/install.log"
    flunk "install of $old"
    exit 1
}
point "$old" # install stages versions/<crate version>; the deploy tracks slots
trust_codex_hooks
"$bin" doctor >"$S/doctor-before.log" 2>&1 && pass "doctor on $old" || {
    cat "$S/doctor-before.log"
    flunk "doctor on $old (sandbox not like the live home)"
}
before_claude=$(claude_version)
before_cmds=$(claude_cmds)
before_codex=$(codex_skills)
echo "before: claude caches $before_claude; claude commands: $before_cmds"
echo "before: codex skills: $before_codex"

# 2. The deploy, as scripts/deploy-drvecon.sh runs it: native deploy, then doctor.
"$builder" deploy "$commit" --repo "$repo" --drain-secs 60 >"$S/deploy.log" 2>&1
sed 's/^/  deploy| /' "$S/deploy.log" | tail -n 25
if grep -q '^result: live' "$S/deploy.log"; then
    pass "deploy of $new went live"
else
    flunk "deploy of $new did not go live"
fi
"$bin" doctor >"$S/doctor-after.log" 2>&1
doctor_rc=$?
sed 's/^/  doctor| /' "$S/doctor-after.log"
[ "$doctor_rc" -eq 0 ] && pass "doctor after deploy" || flunk "doctor after deploy"
after_cmds=$(claude_cmds)
after_codex=$(codex_skills)
echo "after: claude caches $(claude_version); claude commands: $after_cmds"
echo "after: codex skills: $after_codex"
case " $after_cmds " in
*" unvrs:conduct "*) pass "Claude Code init lists /unvrs:conduct" ;;
*) flunk "Claude Code init lacks /unvrs:conduct" ;;
esac
case " $after_codex " in
*" conduct "*) pass "Codex plugin cache has the conduct skill" ;;
*) flunk "Codex plugin cache lacks the conduct skill" ;;
esac

# 3. Rollback (what the wrapper runs when doctor fails): the old plugin comes back.
if [ "$doctor_rc" -eq 0 ]; then
    "$bin" rollback >"$S/rollback.log" 2>&1
    sed 's/^/  rollback| /' "$S/rollback.log" | tail -n 12
    grep -q '^result: live' "$S/rollback.log" && pass "rollback to $old" || flunk "rollback to $old"
    "$bin" doctor >"$S/doctor-rollback.log" 2>&1 && pass "doctor after rollback" || {
        sed 's/^/  doctor| /' "$S/doctor-rollback.log"
        flunk "doctor after rollback"
    }
    [ "$(claude_version)" = "$before_claude" ] && pass "Claude Code back on $before_claude" ||
        flunk "Claude Code caches $(claude_version), not $before_claude"
    [ "$(claude_cmds)" = "$before_cmds" ] && pass "Claude Code commands as before" ||
        flunk "Claude Code commands now: $(claude_cmds)"
    [ "$(codex_skills)" = "$before_codex" ] && pass "Codex skills as before" ||
        flunk "Codex skills now: $(codex_skills)"
fi
[ "$fail" -eq 0 ] && echo "result: every expectation held ($S)" || echo "result: FAILED ($S)"
exit "$fail"
