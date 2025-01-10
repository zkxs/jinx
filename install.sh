#!/bin/bash
RUSTFLAGS='-C target-cpu=native' cargo install --path . --profile release-optimized-debug
