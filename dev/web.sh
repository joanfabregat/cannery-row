#!/usr/bin/env bash
# Vite dev server with hot reload, served at https://cannery.$DEV_DOMAIN
# It proxies /api, /auth and /mcp to the API at CANNERY_API_URL (default: alias
# "api" in the group), so the browser sees one origin and the OIDC callback and
# session cookie work unchanged. Runs attached.
#
# Needs dev/web-fetch.sh, then dev/web-check.sh: this container has the
# network, so it runs no dependency lifecycle scripts; the offline build phase
# (dev/web-check.sh) runs them, and their results stay in web/node_modules.
source "$(dirname "$0")/common.sh"

if [[ ! -d "$REPO/web/node_modules" ]]; then
  echo "web/node_modules is missing: run dev/web-fetch.sh, then dev/web-check.sh, then this script again." >&2
  exit 1
fi

env_file=$(make_env_file "CANNERY_DEV_HOST=cannery.$DEV_DOMAIN" "CANNERY_API_URL=${CANNERY_API_URL:-http://api:8000}")
trap 'rm -f "$env_file"' EXIT

"$RUN" --group "$GROUP" --serve cannery:5173 --project "$REPO/web" --cache-ro npm \
  --env-file "$env_file" --memory 4g --cpus 2 --timeout 8h "$NODE_IMAGE" \
  npm run dev -- --host 0.0.0.0 --port 5173 --strictPort
