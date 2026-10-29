#!/bin/sh
# Build chumpwn for the stock Chumby (ARM926EJ-S, EABI5, soft float).
set -eu
cd "$(dirname "$0")"
cargo build --release --target armv5te-unknown-linux-musleabi
cp target/armv5te-unknown-linux-musleabi/release/chumpwn ./chumpwn
ls -l chumpwn
file chumpwn
