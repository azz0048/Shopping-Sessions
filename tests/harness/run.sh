#!/bin/bash
# run.sh NAME [gen.py args...]
# Generates WORK/d/NAME (if missing), computes the reference answer, runs the
# pipeline on the fast and generic paths, and compares both with the reference.
set -u
NAME=$1; shift
H=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$H/../.." && pwd)
WORK=${WORK:-$ROOT/work}
BIN=${BIN:-$ROOT/target/x86_64-unknown-linux-gnu/release/pipeline}
D=$WORK/d/$NAME; R=$WORK/r/$NAME; O=$WORK/o/$NAME
if [ ! -f "$D/manifest.json" ]; then python3 "$H/gen.py" "$@" --output "$D" || exit 2; fi
[ -f "$R/user_daily.csv" ] || python3 "$H/ref.py" "$D" "$R" || exit 2
st=0
for mode in fast generic; do
  rm -rf "$O.$mode"; mkdir -p "$O.$mode"
  if [ $mode = fast ]; then args=(); else args=(--generic); fi
  DATA_DIR=$D OUTPUT_DIR=$O.$mode NCPU=${NCPU:-8} XL_TIMING=1 "$BIN" "${args[@]}" 2> "$O.$mode.log"
  rc=$?
  fb=$(grep -c FALLBACK "$O.$mode.log")
  tot=$(grep -o 'exit at [0-9.]* ms' "$O.$mode.log" | tail -1)
  res=$(python3 "$H/compare.py" "$O.$mode" "$R" 2>&1); ok=$?
  echo "[$NAME/$mode] rc=$rc fallback=$fb $tot :: $(echo "$res" | grep -E 'OK|MISMATCH' | tr '\n' ' ')"
  [ $ok -ne 0 ] && { echo "$res" | head -40; st=1; }
  [ $fb -ne 0 ] && grep -E "FALLBACK|retry" "$O.$mode.log" | head -3
done
exit $st
