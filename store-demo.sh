#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: ./store-demo.sh\n\nBuilds and opens a disposable desktop store demo with public signed test fixtures.\nNo personal application data, production trust key or public store is used.\nClose the window or press Ctrl-C to stop both demo processes. Test data stays under target/store-demo/.\n'
}
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi
case "${1:-}" in
    -h|--help) usage; exit 0 ;;
    '') if [[ $# -ne 0 ]]; then usage >&2; exit 2; fi ;;
    *) usage >&2; exit 2 ;;
esac

root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$root"
build_environment="$(python3 "$root/scripts/build_layout.py" desktop)"
eval "$build_environment"
target="$(python3 "$root/scripts/build_layout.py" desktop --artifacts release)"
case "$target" in
    /*) ;;
    *) target="$root/$target" ;;
esac
./build.sh desktop
cargo build --locked --release -p peersus-app --features test-plugin --example store_demo

mkdir -p -- target/store-demo
session="$(mktemp -d "$root/target/store-demo/session-XXXXXXXX")"
data="$session/data"
owner_pid=''
ui_pid=''

# Only these unreaped child PIDs belong to this launcher. Never stop another owner.
stop_child() {
    local pid="$1" signal="$2" attempt
    if kill -0 "$pid" 2>/dev/null; then
        kill -"$signal" "$pid" 2>/dev/null || :
        for ((attempt=0; attempt<50; attempt++)); do
            if ! kill -0 "$pid" 2>/dev/null; then break; fi
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || :
            for ((attempt=0; attempt<20; attempt++)); do
                if ! kill -0 "$pid" 2>/dev/null; then break; fi
                sleep 0.1
            done
        fi
        if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || :; fi
    fi
    wait "$pid" 2>/dev/null || :
}
cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if [[ -n "$ui_pid" ]]; then stop_child "$ui_pid" TERM; fi
    if [[ -n "$owner_pid" ]]; then stop_child "$owner_pid" INT; fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

"$target/examples/store_demo" "$data" &
owner_pid=$!
for ((attempt=0; attempt<100; attempt++)); do
    if [[ -S "$data/peersus/control.sock" ]]; then break; fi
    if ! kill -0 "$owner_pid" 2>/dev/null; then
        printf 'Demo owner stopped before becoming ready.\n' >&2
        exit 1
    fi
    sleep 0.1
done
if [[ ! -S "$data/peersus/control.sock" ]]; then
    printf 'Demo owner did not become ready within 10 seconds.\n' >&2
    exit 1
fi
printf 'Isolated store demo: refresh the store, install Echo, then remove it and confirm removal.\n'
printf 'This fixture demonstrates package UX, not file-transfer or clipboard actions.\n'
XDG_DATA_HOME="$data" PEERSUS_NO_AUTOSTART=1 PEERSUS_PAGE=plugins \
    "$target/peersus-ui" &
ui_pid=$!
wait "$ui_pid"
ui_pid=''
