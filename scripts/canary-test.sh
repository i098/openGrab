#!/usr/bin/env bash
# Tests scripts/canary.sh with a stub p0_exit: the result line, the exit code and the alert command.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

cat >"$work/p0_exit" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$STUB_ARGS"
cat "$STUB_OUT"
printf '%s\n' "$STUB_ERR" >&2
exit "$STUB_RC"
STUB
chmod +x "$work/p0_exit"
cat >"$work/alert" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$work/alerts"
STUB
chmod +x "$work/alert"

# run STDOUT RC STDERR [LINK]: runs canary.sh once against the stub. Its last result line goes to $work/last.
run() {
    rm -rf "$work/state" "$work/alerts"
    printf '%b' "$1" > "$work/stdout"
    STUB_OUT="$work/stdout" STUB_RC="$2" STUB_ERR="$3" STUB_ARGS="$work/args" \
        PARLEY_CANARY_BIN="$work/p0_exit" PARLEY_CANARY_LINK="${4-https://meet.google.com/abc-defg-hij}" \
        PARLEY_CANARY_STATE="$work/state" PARLEY_CANARY_ALERT_CMD="$work/alert" \
        "$root/scripts/canary.sh" >/dev/null 2>&1
    rc=$?
    tail -n 1 "$work/state/canary.log" > "$work/last"
}

# expect NAME DETAIL COMMAND...: passes when COMMAND succeeds.
expect() {
    local name=$1 detail=$2
    shift 2
    if "$@"; then echo "ok   $name"; else echo "FAIL $name: $detail"; failures=$((failures + 1)); fi
}

stamp='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z'
# The driver's real labels: the outsider check prints as outsider-input.
pass_out='PASS join: 1 of 1 joins reached the call\nPASS chat: 10 lines read\nPASS tile: both states\nPASS outsider-input: 1 other participant\n'

run "$pass_out" 0 ""
expect "pass exits 0" "rc=$rc" test "$rc" -eq 0
expect "pass line" "$(cat "$work/last")" grep -Eq "^PASS $stamp checks=join,chat,tile,outsider evidence=" "$work/last"
expect "pass runs the four checks on the attached port" "$(cat "$work/args")" grep -qxF -- join,chat,tile,outsider "$work/args"
expect "pass sends no alert" "alert ran" test ! -e "$work/alerts"

run "PASS join: ok\nFAIL chat: 3 of 10 lines read\nPASS tile: ok\nPASS outsider-input: ok\n" 1 ""
expect "failed check exits 1" "rc=$rc" test "$rc" -eq 1
expect "failed check names the check" "$(cat "$work/last")" grep -Eq "^FAIL $stamp failed=chat exit=1 " "$work/last"
expect "failed check runs the alert with the line" "$(cat "$work/alerts" 2>/dev/null)" cmp -s "$work/last" "$work/alerts"

run "PASS join: ok\nPASS chat: ok\nPASS tile: ok\n" 0 ""
expect "exit 0 without the outsider check is a FAIL" "$(cat "$work/last")" \
    grep -Eq "^FAIL $stamp failed=run exit=0 " "$work/last"

run "" 1 "Error: no Chromium answers on CDP port 9523 https://meet.google.com/abc-defg-hij me@example.com"
expect "run error is a FAIL with a reason" "$(cat "$work/last")" \
    grep -Fq "failed=run exit=1 reason=Error: no Chromium answers on CDP port 9523 <url> <address> " "$work/last"

run "" 0 "" ""
expect "missing link is a config FAIL" "$(cat "$work/last")" \
    grep -Fq " failed=config reason=PARLEY_CANARY_LINK is not set" "$work/last"
expect "missing link exits 1" "rc=$rc" test "$rc" -eq 1
expect "missing link alerts" "alert not run" test -s "$work/alerts"

if ((failures)); then echo "$failures failed"; exit 1; fi
echo "all canary tests passed"
