#!/usr/bin/env bash
# Source this before using cargo/rustc for lsdoc.
#
# The Rust toolchain lives on the persistent /aux mount (NOT ~/.cargo, which is
# wiped on container rebuild). This is the SAME shared toolchain Tine uses, but
# lsdoc is otherwise standalone — it does not depend on Tine's env.sh and pulls
# in none of Tine's browser/Playwright machinery.
export CARGO_HOME=/aux/koutecky/logseq/.toolchain/cargo
export RUSTUP_HOME=/aux/koutecky/logseq/.toolchain/rustup
export PATH="$CARGO_HOME/bin:$PATH"
