#!/usr/bin/env bash
# Daily canary: the Phase 0 exit check (join, chat, tile, outsider) on the attached, signed-in Chromium.
# Appends one PASS or FAIL line per run to $PARLEY_CANARY_STATE/canary.log. A FAIL runs $PARLEY_CANARY_ALERT_CMD
# with that line as its only argument, then exits 1. Settings: deploy/systemd/ and crates/parley-join/README.md.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin=${PARLEY_CANARY_BIN:-$root/target/release/p0_exit}
link=${PARLEY_CANARY_LINK:-}
port=${PARLEY_CANARY_CDP_PORT:-9523}
host_authuser=${PARLEY_CANARY_HOST_AUTHUSER:-1}
bot_authuser=${PARLEY_CANARY_BOT_AUTHUSER:-0}
limit=${PARLEY_CANARY_TIMEOUT:-1800}
state=${PARLEY_CANARY_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/parley}
alert=${PARLEY_CANARY_ALERT_CMD:-}
checks=join,chat,tile,outsider
stamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
(umask 077 && mkdir -p "$state/logs" "$state/runs")
log="$state/logs/$stamp.txt"

finish() {
    local line=$1 rc=0
    printf '%s\n' "$line" | tee -a "$state/canary.log"
    if [[ $line == FAIL* ]]; then
        rc=1
        if [[ -n $alert ]] && ! "$alert" "$line"; then
            echo "canary: alert command failed" >&2
        fi
    fi
    exit "$rc"
}

if [[ -z $link ]]; then
    finish "FAIL $stamp failed=config reason=PARLEY_CANARY_LINK is not set"
fi
if [[ ! -x $bin ]]; then
    finish "FAIL $stamp failed=config reason=no executable at $bin"
fi

timeout --kill-after=60 "$limit" "$bin" "$link" --attach "$port" --checks "$checks" \
    --host-authuser "$host_authuser" --bot-authuser "$bot_authuser" --out "$state/runs/$stamp" \
    >"$log" 2>&1
rc=$?

# The driver labels the outsider check "outsider-input", not "outsider".
passed=$(sed -n 's/^PASS \([a-z-]*\):.*/\1/p' "$log" | sort | paste -sd, -)
failed=$(sed -n 's/^FAIL \([a-z-]*\):.*/\1/p' "$log" | sort | paste -sd, -)
if [[ $rc == 0 && $passed == chat,join,outsider-input,tile ]]; then
    finish "PASS $stamp checks=$checks evidence=$state/runs/$stamp"
fi
if [[ -n $failed ]]; then
    finish "FAIL $stamp failed=$failed exit=$rc log=$log"
fi
reason=$(grep -v '^$' "$log" | tail -n 1 \
    | sed -E 's#https?://[^[:space:]]*#<url>#g; s#[^[:space:]]+@[^[:space:]]+#<address>#g' | cut -c1-200)
finish "FAIL $stamp failed=run exit=$rc reason=${reason:-no output} log=$log"
