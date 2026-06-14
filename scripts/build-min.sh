#!/bin/sh
# Build the smallest possible release binary: the size-tuned [profile.release]
# (Cargo.toml) plus a from-source std/core built with the immediate-abort panic
# strategy (no panic formatting machinery) and optimize_for_size.
#
# Requires: nightly toolchain + the rust-src component
#   rustup toolchain install nightly && rustup component add rust-src --toolchain nightly
#
# Output: target/<host-triple>/release/nano
set -e

TARGET=$(rustc -vV | sed -n 's/host: //p')

# -Cpanic=immediate-abort: panics call abort() immediately with no message/format
# code (much smaller than panic=abort). Needs -Zbuild-std so core/std use it too.
# (On older nightlies this was the build-std feature `panic_immediate_abort`; if
#  this nightly rejects `optimize_for_size`, drop that -Z flag.)
export RUSTFLAGS="-Zunstable-options -Cpanic=immediate-abort${RUSTFLAGS:+ $RUSTFLAGS}"

cargo +nightly build --release \
    -Z build-std=std,panic_abort \
    -Z build-std-features=optimize_for_size \
    --target "$TARGET" "$@"

echo "Built: target/$TARGET/release/nano  ($(stat -c%s "target/$TARGET/release/nano") bytes)"
