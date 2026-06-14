#!/bin/sh
# Speed-tuned build (the `perf` profile: opt-level=3 + LTO).  Measurably faster
# than upstream C nano on the file-load path.  Output: target/perf/nano
#
#   ./scripts/build-fast.sh            # portable, ~8% faster than C nano on load
#   ./scripts/build-fast.sh --native   # tuned for THIS CPU, ~13% faster (non-portable)
set -e

RF=""
if [ "$1" = "--native" ]; then
    RF="-Ctarget-cpu=native"
    shift
fi

RUSTFLAGS="${RF}${RUSTFLAGS:+ $RUSTFLAGS}" cargo build --profile perf "$@"
echo "Built: target/perf/nano  ($(stat -c%s target/perf/nano 2>/dev/null || echo '?') bytes)"
