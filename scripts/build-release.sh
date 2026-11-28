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
# This checkout's target folder, or the one asked for in so many words (LAPSEWERK_TARGET_DIR, the app bundle's):
# an inherited CARGO_TARGET_DIR (another worktree's) built there and left target/release here as it was, while
# this script said "built" (two test rounds ran on old binaries).
export CARGO_TARGET_DIR="${LAPSEWERK_TARGET_DIR:-$here/target}"
# --locked: a Cargo.lock that does not match Cargo.toml stops here (a commit once carried the wrong one).
cargo build --release --locked -p lapsewerk-cli -p lapsewerk-app "$@"
echo "built: $CARGO_TARGET_DIR/release/lapsewerk, $CARGO_TARGET_DIR/release/lapsewerk-app"
