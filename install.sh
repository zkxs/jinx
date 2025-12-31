#!/bin/bash
export RUSTFLAGS='-C target-cpu=native'
cargo auditable install --locked --path . --profile release-optimized-debug
