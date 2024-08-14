#!/bin/bash
set -o pipefail
while jinx | tee -a jinx.log; do
  true
done
