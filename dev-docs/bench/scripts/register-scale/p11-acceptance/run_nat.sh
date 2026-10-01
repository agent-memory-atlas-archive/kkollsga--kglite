#!/bin/bash
# usage: run_nat.sh VENV_DIR LABEL MODE [ROWS] -- APFS clone of the built national graph ($P11_GRAPH), probe under the 10.5 GB watchdog.
# env: P11_GRAPH (pristine graph), P11_OUT (results dir), P11_WORK (clone parent). Writes ${LABEL}_${MODE}.{json,err,wdlog,wd,state}.
D=$(cd "$(dirname "$0")" && pwd)
VENV=$1; LABEL=$2; MODE=$3; ROWS=${4:-1000}
W=$P11_WORK/work_$LABEL; rm -rf $W; cp -cR $P11_GRAPH $W
echo "# $(date +%H:%M:%S) $LABEL nat $MODE $(uptime | sed 's/.*load/load/') rustc=$(pgrep -x rustc | wc -l | tr -d ' ') top: $(ps -Ao pcpu,comm -r | sed -n 2,4p | awk '{printf "%s %s; ", $1, $2}' | cut -c1-120)" >> $P11_OUT/state.log
KGLITE_LOAD_TIMING=1 $VENV/bin/python $D/wd.py $P11_OUT/${LABEL}_${MODE}.wdlog -- $VENV/bin/python $D/save_probe_nat.py $W ${LABEL}_$MODE $MODE $ROWS > $P11_OUT/${LABEL}_${MODE}.wd 2>&1
grep '^{' $P11_OUT/${LABEL}_${MODE}.wdlog | tail -1 > $P11_OUT/${LABEL}_${MODE}.json
grep '^\[TIMING\]' $P11_OUT/${LABEL}_${MODE}.wdlog > $P11_OUT/${LABEL}_${MODE}.err
rm -rf $W
