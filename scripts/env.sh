#!/usr/bin/env bash
# Source this before using cargo/rustc for lsdoc.
#
# On the primary dev machine the Rust toolchain lives on the persistent /aux
# mount (NOT ~/.cargo, which is wiped on container rebuild). This is the SAME
# shared toolchain Tine uses, but lsdoc is otherwise standalone — it does not
# depend on Tine's env.sh and pulls in none of Tine's browser/Playwright
# machinery.
#
# On any other machine (e.g. running tools/graph-check.mjs on a real graph) that
# toolchain path does not exist, so we fall back to whatever cargo/rustc is
# already on PATH. Override the root with LSDOC_TOOLCHAIN_ROOT if needed.
_lsdoc_toolchain_root="${LSDOC_TOOLCHAIN_ROOT:-/aux/koutecky/logseq/.toolchain}"
if [ -d "$_lsdoc_toolchain_root/cargo" ] && [ -d "$_lsdoc_toolchain_root/rustup" ]; then
  export CARGO_HOME="$_lsdoc_toolchain_root/cargo"
  export RUSTUP_HOME="$_lsdoc_toolchain_root/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
elif ! command -v cargo >/dev/null 2>&1; then
  echo "lsdoc env.sh: no /aux toolchain and no cargo on PATH; install rustup (https://rustup.rs) or set LSDOC_TOOLCHAIN_ROOT" >&2
fi
unset _lsdoc_toolchain_root
