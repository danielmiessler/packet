#!/bin/zsh
# Measures the idle CPU and physical memory footprint of the running app.
set -euo pipefail
pid=$(pgrep -x packet) || { echo "packet not running"; exit 1; }
case ${1:-} in
  cpu) total=0; for i in {1..60}; do c=$(ps -o %cpu= -p $pid); total=$(bun -e "console.log($total+$c)"); sleep 1; done
       mean=$(bun -e "console.log(($total/60).toFixed(3))"); echo "mean cpu ${mean}%"; bun -e "process.exit($mean < 0.5 ? 0 : 1)";;
  mem) mb=$(footprint -p $pid | awk '/phys_footprint:/{print $2; exit}')
       [[ $mb == <-> ]] || { echo "could not read footprint"; exit 2; }
       echo "phys_footprint ${mb} MB"; (( mb < 50 ));;
  *) echo "usage: probe-footprint.sh cpu|mem"; exit 2;;
esac
