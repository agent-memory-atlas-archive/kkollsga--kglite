#!/bin/bash
. /private/tmp/claude-501/-Volumes-EksternalHome-Koding-Rust-KGLite/64ba7eb6-7f70-4ffa-ad66-43014d2e72b1/scratchpad/study/t1/p11/env.sh
cd $S
for m in noop set append; do for rep in 1 2; do
  if [ $rep = 1 ]; then order="cand ref"; else order="ref cand"; fi
  for w in $order; do $D/run_nat.sh $S/$w ${w}_r${rep}_$m $m 1000; done
done; done
echo finished > $S/out/matrix.done
