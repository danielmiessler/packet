#!/bin/zsh
# Checks that the last 10 stored rounds are 55–65 s apart (restarts and Test Now rounds, under 45 s, are skipped).
set -euo pipefail
db="$HOME/Library/Application Support/Packet/packet.sqlite"
gaps=$(sqlite3 "$db" "with r as (select at_ms from rounds order by at_ms desc limit 11) select (at_ms - lag(at_ms) over (order by at_ms))/1000.0 from r" | rg -v '^$')
echo "gaps (s): $(print -r -- "$gaps" | tr '\n' ' ')"
print -r -- "$gaps" | bun -e 'const g=(await Bun.stdin.text()).trim().split("\n").map(Number).filter(x=>x>45); process.exit(g.length>=5 && g.every(x=>x>=55&&x<=65)?0:1)'
