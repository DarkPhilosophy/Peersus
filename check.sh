#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: ./check.sh [gate|fmt|metadata|check|clippy|test|audit]\n\nDefault: full strict gate, including the dependency audit. No automatic fixes or audit exceptions.\n'
}
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi
case "${1:-gate}" in
    -h|--help) usage; exit 0 ;;
    gate|fmt|metadata|check|clippy|test|audit) task="${1:-gate}" ;;
    *) usage >&2; exit 2 ;;
esac

root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$root"
build_environment="$(python3 "$root/scripts/build_layout.py" desktop)"
eval "$build_environment"
exec cargo xtask "$task"
