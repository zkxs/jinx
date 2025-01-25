#!/bin/bash
RUSTFLAGS='-C target-cpu=native' cargo install --locked --path . --profile release-optimized-debug
