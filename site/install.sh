#!/bin/sh
# Installs the Peersus development prerelease for the person running it, on Linux (x86-64):
#
#   curl -fsSL https://peersus.axxa.dev/install.sh | sh
#   wget -qO- https://peersus.axxa.dev/install.sh | sh
#
# Options after `sh -s --` go to `peersus install` (`--prefix DIR`, default ~/.local).
#
# What it does, and nothing else: it downloads the list of the newest development release's files with their SHA-256
# (`SHA256SUMS`) and its signature, staged with this site from that release, checks the signature with the publisher's key
# written below, downloads the Linux archive the list names from the GitHub release its signed name gives, checks it against
# the list, unpacks it in a temporary folder and runs `peersus install` from it. Nothing runs before every check holds. No
# administrator rights; no data is touched. Afterwards the application keeps itself current.
set -eu

LIST="https://peersus.axxa.dev/app"
RELEASES="https://github.com/DarkPhilosophy/Peersus/releases/download"
# The publisher's Ed25519 key for the development channel (the one development builds verify their updates with), as the
# SubjectPublicKeyInfo openssl reads. A public key is not a secret.
PUBLIC_KEY="MCowBQYDK2VwAyEATkjs8+yUosIiQmN5r4YZ5cZwIZMYkRAt4PdS0SiqotI="
# What the signature covers before the list: its purpose, so another of the publisher's signatures is never this one.
CONTEXT="peersus-sums-v1"

say() { printf 'peersus: %s\n' "$*" >&2; }
fail() { say "$*"; exit 1; }

[ "$(uname -s)" = Linux ] || fail "this installer is for Linux; see https://peersus.axxa.dev/install.html for other systems"
case "$(uname -m)" in
    x86_64 | amd64) ARCHITECTURE=x86_64 ;;
    *) fail "there is no prerelease for $(uname -m) yet; build it from source: https://peersus.axxa.dev/install.html" ;;
esac
[ "$(id -u)" != 0 ] || fail "run this as yourself, not as root: Peersus installs for one person, in their own folder"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto '=https' --tlsv1.2 --retry 2 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q --https-only -O "$2" "$1"; }
else
    fail "curl or wget is needed to download"
fi
for tool in openssl sha256sum tar mktemp; do
    command -v "$tool" >/dev/null 2>&1 || fail "$tool is needed and was not found"
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/peersus-install.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT INT TERM
cd "$WORK"

say "downloading the list of the prerelease's files"
fetch "$LIST/SHA256SUMS" SHA256SUMS || fail "the list of the prerelease's files could not be downloaded"
fetch "$LIST/SHA256SUMS.sig" SHA256SUMS.sig || fail "its signature could not be downloaded"

{
    printf '%s\n' '-----BEGIN PUBLIC KEY-----'
    printf '%s\n' "$PUBLIC_KEY"
    printf '%s\n' '-----END PUBLIC KEY-----'
} > publisher.pem
{ printf '%s\n' "$CONTEXT"; cat SHA256SUMS; } > signed
openssl pkeyutl -verify -pubin -inkey publisher.pem -rawin -in signed -sigfile SHA256SUMS.sig >/dev/null 2>&1 \
    || fail "the list is not signed by the Peersus publisher (or this openssl is older than 3.0); nothing was installed"

# Authentic from here on: the one Linux archive of this machine's architecture, by its exact line, named
# `peersus-<version>-<channel>.<build>-linux-<architecture>.tar.gz`; a program's line (`<archive>/<program>`) is passed over.
LINE=""
while IFS= read -r line; do
    case "$line" in
        */*) ;;
        *"  peersus-"*"-linux-$ARCHITECTURE.tar.gz")
            [ -z "$LINE" ] || fail "the list names more than one Linux $ARCHITECTURE archive"
            LINE="$line" ;;
    esac
done < SHA256SUMS
[ -n "$LINE" ] || fail "the prerelease has no Linux $ARCHITECTURE archive"
FILE="${LINE#*  }"
RELEASE="${FILE#peersus-}"
RELEASE="${RELEASE%-linux-"$ARCHITECTURE".tar.gz}"
case "$FILE" in
    .* | *" "*) fail "the archive's name is not a plain file name" ;;
esac
case "$RELEASE" in
    *[!0-9a-z.-]* | "" | -* | *-*-*) fail "the archive's name does not say its release" ;;
esac

say "downloading $FILE"
fetch "$RELEASES/v$RELEASE/$FILE" "$FILE" || fail "the archive could not be downloaded"
printf '%s\n' "$LINE" | sha256sum -c --status - \
    || fail "the archive is not the one the signed list names; nothing was installed"

mkdir programs
tar -xzf "$FILE" -C programs --no-same-owner
[ -x programs/peersus ] || fail "the archive holds no peersus program"
# The signed list goes with the programs: `peersus install` checks each program against it and keeps it, and the
# application checks the installation against it at each start and puts back a program that is missing or damaged.
cp SHA256SUMS SHA256SUMS.sig programs/
say "verified; installing"
programs/peersus install "$@"
