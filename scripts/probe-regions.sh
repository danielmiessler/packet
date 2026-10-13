#!/bin/zsh
# Checks that the latest round reached at least 6 regions across at least 4 continents.
set -euo pipefail
/Applications/Packet.app/Contents/MacOS/packet now --json | jq -e '[.regions[] | select(.ms != null)] as $up | ($up | length) >= 6 and ([$up[].continent] | unique | length) >= 4' >/dev/null && echo "regions ok"
