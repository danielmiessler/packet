#!/bin/zsh
# Checks that every hostname or URL in the source is on the declared target list.
set -euo pipefail
cd "${0:A:h}/.."
allowed='^(speed\.cloudflare\.com|1\.1\.1\.1|dynamodb\.[a-z0-9-]+\.amazonaws\.com|www\.apple\.com|www\.google\.com|www\.cloudflare\.com|github\.com|localhost|example\.com)$'
found=$(rg -o --no-filename '"(https?://)?([a-z0-9-]+\.)+(com|net|org|io|dev|app|cloud)(/[^"]*)?"' src | tr -d '"' | sed -E 's#^https?://##; s#/.*$##' | sort -u)
extra=$(print -r -- "$found" | rg -v "$allowed" || true)
[[ -z $extra ]] || { echo "undeclared hosts: $extra"; exit 1; }
echo "hosts in src: $(print -r -- "$found" | tr '\n' ' ')"
