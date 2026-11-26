#!/bin/sh
set -eu
# Explicit installer: invoked by CI or deliberately by a developer, never by the gate.
sudo apt-get update
sudo apt-get install -y build-essential clang libclang-dev llvm-dev cmake ninja-build pkg-config python3 git curl ca-certificates libfontconfig1-dev libfreetype6-dev libgl1-mesa-dev libegl1-mesa-dev libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxi-dev libxrandr-dev libxcb1-dev libdbus-1-dev libssl-dev libasound2-dev
