#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: ./website.sh [--empty-store]\n\nStages a fresh unpublished website preview and serves it on loopback only.\nDefault: clearly labelled public test fixtures under store-test/, never the production store/.\nUse --empty-store to exercise the normal unpublished empty-store page.\nNo GitHub settings, publishing, credentials or application trust keys are changed.\nCtrl-C stops the server; preview files remain under target/site-preview/.\n'
}
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi
catalog=(--test-catalog)
case "${1:-}" in
    -h|--help) usage; exit 0 ;;
    --empty-store) catalog=() ;;
    '') if [[ $# -ne 0 ]]; then usage >&2; exit 2; fi ;;
    *) usage >&2; exit 2 ;;
esac
root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$root"
mkdir -p -- target/site-preview
session="$(mktemp -d "$root/target/site-preview/session-XXXXXXXX")"
python3 site/build.py --output "$session/site" "${catalog[@]}"
printf 'Offline development preview only. Use the local address printed by the server.\n'
exec python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$session/site"
