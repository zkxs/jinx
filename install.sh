#!/bin/bash
export RUSTFLAGS='-C target-cpu=native'
cp -a ~/.cargo/bin/{jinx,previous-jinx}
cargo auditable install --locked --path . --profile release-optimized-debug
