#!/usr/bin/env bash
# Phase 0 exit test, Meet join. Usage: scripts/p0-exit.sh <meet-link> [p0-exit options]
# Runs the non-headless Chromium inside Xvfb (plan risk R1). See crates/parley-join/README.md.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
command=(cargo run --release -p parley-join --bin p0_exit -- "$@")
attach=false
speakers=false
checks=,
for ((i = 1; i <= $#; i++)); do
    arg=${!i}
    case "$arg" in
        --attach|--attach=*) attach=true ;;
        --speakers|--speakers=*) speakers=true ;;
        --checks=*) checks+=${arg#*=}, ;;
        --checks) ((i += 1)); checks+=${!i-}, ;;
        --) break ;;
    esac
done
if "$attach" && { ! "$speakers" || [[ $checks != , && $checks != *,speakers,* ]]; }; then
    exec "${command[@]}"
fi
exec xvfb-run -a -s "-screen 0 1280x800x24" "${command[@]}"
