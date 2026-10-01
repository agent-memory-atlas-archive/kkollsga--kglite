#!/bin/bash
# usage: runone.sh VENV LABEL SCRIPT [extra args...]  -- clone P11_GRAPH, run SCRIPT GRAPH LABEL args under the watchdog, delete the clone
. /private/tmp/claude-501/-Volumes-EksternalHome-Koding-Rust-KGLite/64ba7eb6-7f70-4ffa-ad66-43014d2e72b1/scratchpad/study/t1/p11/env.sh
VENV=$S/$1; LABEL=$2; SCR=$3; shift 3
W=$P11_WORK/work_$LABEL; rm -rf $W; cp -cR $P11_GRAPH $W
echo "# $(date +%H:%M:%S) $LABEL $SCR $(uptime | sed 's/.*load/load/') rustc=$(pgrep -x rustc | wc -l | tr -d ' ') top: $(ps -Ao pcpu,comm -r | sed -n 2,5p | awk '{printf "%s %s; ", $1, $2}' | cut -c1-200)" >> $P11_OUT/state.log
KGLITE_LOAD_TIMING=1 $VENV/bin/python $D/wd.py $P11_OUT/${LABEL}.wdlog -- $VENV/bin/python $D/$SCR $W $LABEL "$@" > $P11_OUT/${LABEL}.wd 2>&1
grep '^{' $P11_OUT/${LABEL}.wdlog | tail -1 > $P11_OUT/${LABEL}.json
rm -rf $W
