#!/usr/bin/env bash
# Fetch large, public, reproducible benchmark corpora into bench/corpus/ (gitignored).
#
# Nothing here is committed — this script just clones public graphs so the
# throughput comparison (cargo run) has "something reasonably large" to chew on.
#
#   md  : github.com/logseq/docs  — Logseq's own docs, authored *as* a real
#         Logseq Markdown graph (pages/ + journals/). Faithful to real usage.
#   org : git.sr.ht/~bzg/worg     — Worg, a large real Org corpus (hundreds of
#         .org files). GitHub mirror github.com/aspiers/worg as fallback.
#
# Shallow clones (--depth 1); re-runnable (skips what's already present).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
corpus="$here/corpus"
mkdir -p "$corpus"

clone() { # dest url [fallback_url]
  local dest="$corpus/$1"; shift
  if [ -d "$dest/.git" ]; then
    echo "✓ $dest already present (skipping)"
    return 0
  fi
  local url="$1"; shift
  echo "→ cloning $url → $dest"
  if git clone --depth 1 "$url" "$dest" 2>/dev/null; then
    return 0
  fi
  if [ $# -ge 1 ]; then
    echo "  primary failed; trying fallback $1"
    git clone --depth 1 "$1" "$dest"
  else
    echo "  clone failed: $url" >&2
    return 1
  fi
}

# Markdown: Logseq docs graph
clone logseq-docs https://github.com/logseq/docs

# Org: Worg (sourcehut primary, GitHub mirror fallback)
clone worg https://git.sr.ht/~bzg/worg https://github.com/aspiers/worg

echo
echo "Corpus sizes:"
du -sh "$corpus"/*/ 2>/dev/null || true
echo
echo "Markdown files (logseq-docs): $(find "$corpus/logseq-docs" -name '*.md' 2>/dev/null | wc -l)"
echo "Org files      (worg)       : $(find "$corpus/worg" -name '*.org' 2>/dev/null | wc -l)"
echo
echo "Now run e.g.:"
echo "  cargo run --release -- --graph corpus/logseq-docs"
echo "  cargo run --release -- --graph corpus/worg --format org"
