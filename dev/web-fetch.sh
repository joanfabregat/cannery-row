#!/usr/bin/env bash
# Fetch phase for the web app: install from the lockfile with lifecycle scripts
# disabled, then audit. Network and a writable npm cache; nothing from a
# dependency is executed. To add a package, run npm install --ignore-scripts
# --save-exact <pkg>@<version> the same way, after checking the package.
source "$(dirname "$0")/common.sh"

"$RUN" --network --project "$REPO/web" --cache npm --memory 4g --cpus 2 --timeout 30m \
  "$NODE_IMAGE" /bin/sh -c '
    set -eu
    npm ci --ignore-scripts --no-fund
    npm audit --audit-level=high
  '
