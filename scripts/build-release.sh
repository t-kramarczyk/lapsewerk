#!/bin/sh
# Builds the release programs (target/release/lapsewerk, lapsewerk-app) as they are handed out:
# without the build machine's paths inside (home folder, checkout, Cargo's and rustup's folders),
# so the programs say nothing about who built them where, and two builds compare.
# Usage: scripts/build-release.sh [cargo build options]
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
# LightCraft beside the checkout (a path dependency): named as such.
lightcraft=$(cd "$here/.." && pwd)
# The last matching prefix wins: the general ones first, the checkout last.
RUSTFLAGS="${RUSTFLAGS:-} \
--remap-path-prefix=$HOME=home \
--remap-path-prefix=$lightcraft=src \
--remap-path-prefix=$cargo_home=cargo \
--remap-path-prefix=$rustup_home=rustup \
--remap-path-prefix=$here=lapsewerk"
export RUSTFLAGS
cd "$here"
cargo build --release -p lapsewerk-cli -p lapsewerk-app "$@"
echo "built: target/release/lapsewerk, target/release/lapsewerk-app"
