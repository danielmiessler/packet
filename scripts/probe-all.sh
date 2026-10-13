#!/bin/zsh
# Runs every live probe against the installed app.
set -uo pipefail
cd "${0:A:h}"
fail=0
for p in "probe-schema.sh" "probe-hosts.sh" "probe-regions.sh" "probe-cadence.sh" "probe-footprint.sh mem" "probe-footprint.sh cpu"; do
  print -n "$p: "; if zsh ./${=p}; then :; else fail=1; echo "  FAILED"; fi
done
exit $fail
