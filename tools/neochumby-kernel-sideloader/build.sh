#!/bin/sh
# Build the kernel sideloader for the stock Chumby (ARM926EJ-S, EABI5, soft float).
set -eu
cd "$(dirname "$0")"
cargo build --release --target armv5te-unknown-linux-musleabi
cp target/armv5te-unknown-linux-musleabi/release/neochumby-kernel-sideloader \
    ./neochumby-kernel-sideloader
ls -l neochumby-kernel-sideloader
file neochumby-kernel-sideloader
