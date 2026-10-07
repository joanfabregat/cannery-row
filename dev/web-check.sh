#!/usr/bin/env bash
# Build phase for the web app, offline: lifecycle scripts, lint, type check,
# tests, generated-types check and production build. Needs dev/web-fetch.sh.
source "$(dirname "$0")/common.sh"

"$RUN" --project "$REPO/web" --cache-ro npm --memory 4g --cpus 2 --timeout 30m \
  "$NODE_IMAGE" /bin/sh -c '
    set -eu
    export npm_config_nodedir=/usr/local
    npm rebuild --silent
    npm run --silent lint
    npm run --silent typecheck
    npm run --silent test
    npm run --silent api:types:check
    npm run --silent build
  '
