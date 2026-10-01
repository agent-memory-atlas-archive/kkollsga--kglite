#!/bin/bash
. /private/tmp/claude-501/-Volumes-EksternalHome-Koding-Rust-KGLite/64ba7eb6-7f70-4ffa-ad66-43014d2e72b1/scratchpad/study/t1/p11/env.sh
cd $S
mv out/delivery_cand.json out/delivery_cand_r1.json; mv out/delivery_cand.wdlog out/delivery_cand_r1.wdlog; mv out/delivery_cand.wd out/delivery_cand_r1.wd
./runone.sh ref delivery_ref_r1 delivery_nat.py
./runone.sh cand delivery_cand_r2 delivery_nat.py
./runone.sh ref delivery_ref_r2 delivery_nat.py
./runone.sh ref reopen_ref_r1 reopen_nat.py 24739020
./runone.sh cand reopen_cand_r2 reopen_nat.py 24739020
echo finished > out/matrix3.done
