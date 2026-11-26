#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: ./build.sh [desktop|android]\n\nBuilds local release artifacts; does not install, sign, publish or deploy.\n'
}
if [[ $# -gt 1 ]]; then usage >&2; exit 2; fi
case "${1:-desktop}" in
    -h|--help) usage; exit 0 ;;
    desktop) task=build; surface=desktop ;;
    android) task=android; surface=android ;;
    *) usage >&2; exit 2 ;;
esac

root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd -- "$root"
build_environment="$(python3 "$root/scripts/build_layout.py" "$surface")"
eval "$build_environment"
exec cargo xtask "$task"
