#!/usr/bin/env bash
# Regenerate the complete Rust handler/serde DTO contract and web client types.
# Build web/dist first so the Rust server's embedded assets are available.
source "$(dirname "$0")/common.sh"

temporary=$(mktemp /tmp/cannery-openapi.XXXXXXXX)
trap 'rm -f "$temporary"' EXIT
"$RUN_RUST" -C "$REPO" run --locked -p cannery-server \
  --bin openapi-generated > "$temporary"
mv "$temporary" "$REPO/web/openapi.json"
"$RUN" --project "$REPO/web" --cache-ro npm --memory 2g --timeout 15m "$NODE_IMAGE" \
  npm run --silent api:types
