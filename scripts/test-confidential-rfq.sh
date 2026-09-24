#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

# Build the host separately to avoid linking it with the consumer's no-entrypoint feature.
cargo build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path zama/solana/programs/zama-host/Cargo.toml \
  --sbf-out-dir "$PWD/target/deploy"
cargo build-sbf --tools-version v1.57 --arch v3 \
  --manifest-path programs/confidential_rfq/Cargo.toml \
  --sbf-out-dir "$PWD/target/deploy"
cargo test --locked -p confidential_rfq "$@"
