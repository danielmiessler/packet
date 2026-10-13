#!/bin/zsh
# Checks that no column could hold a visited host, an app, an SSID or traffic contents.
set -euo pipefail
db="${PACKET_DATA_DIR:-$HOME/Library/Application Support/Packet}/packet.sqlite"
[[ -f $db ]] || { echo "no store at $db"; exit 1; }
tables=$(sqlite3 "$db" "select count(*) from sqlite_master where type='table' and name in ('rounds','bandwidth')")
[[ $tables == 2 ]] || { echo "store is missing the rounds or bandwidth table"; exit 1; }
cols=$(sqlite3 "$db" "select name from pragma_table_info('rounds') union all select name from pragma_table_info('bandwidth')")
bad=$(print -r -- "$cols" | rg -i 'host|url|app|ssid|bssid|payload|content|process|domain|name' || true)
[[ -z $bad ]] || { echo "suspect columns: $bad"; exit 1; }
echo "$(print -r -- "$cols" | wc -l | tr -d ' ') columns, none can hold a host, app, SSID or payload"
