#!/usr/bin/env bash
# Cannery Row as one self-contained binary: the native `cannery`
# with its bundled PostgreSQL, served by run-podman --serve at
# https://$NAME.$DEV_DOMAIN, with sign-in through the OIDC client in
# dev/local.env.
#
#   dev/local.sh build   web app, PostgreSQL bundle (if missing) and a release
#                        cannery with the bundled-postgres feature
#   dev/local.sh serve   run it attached for up to 8 hours; Ctrl+C stops it
#   dev/local.sh         both
#
# Everything the instance keeps (database, unpacked PostgreSQL, objects) is
# under $STATE/data; deleting it resets the instance. The served binary is a
# copy under $STATE/bin, so later runs in the checkout cannot change it.
source "$(dirname "$0")/common.sh"

NAME=${CANNERY_LOCAL_NAME:-cannery}
STATE=$STATE_ROOT/cannery-local
RUST_IMAGE=docker.io/library/rust:1-bookworm
SERVE_IMAGE=docker.io/library/debian:trixie-slim
PG_VERSION=$(sed -n 's/^PG_VERSION=//p' "$REPO/dev/postgres-bundle-build.sh")
BUNDLE_DIR=$REPO/target/pg-bundle

# Env files hold secrets; remove whichever this run wrote, however it ends.
env_file=
trap '[[ -z "$env_file" ]] || rm -f "$env_file"' EXIT

build() {
  if [[ ! -f "$REPO/web/dist/index.html" ]]; then
    "$REPO/dev/web-fetch.sh"
    "$REPO/dev/web-check.sh"
  fi

  local bundle
  bundle=$(find "$BUNDLE_DIR" -maxdepth 1 -name "postgresql-$PG_VERSION-linux-*.tar.zst" 2>/dev/null | head -1 || true)
  if [[ -z "$bundle" ]]; then
    rm -rf "$STATE/bundle"
    "$REPO/dev/build-postgres-bundle.sh" "$STATE/bundle"
    mkdir -p "$BUNDLE_DIR"
    cp "$STATE"/bundle/postgresql-"$PG_VERSION"-linux-*.tar.zst* "$BUNDLE_DIR/"
    rm -rf "$STATE/bundle"
    bundle=$(find "$BUNDLE_DIR" -maxdepth 1 -name "postgresql-$PG_VERSION-linux-*.tar.zst" | head -1)
  fi

  # Fetch phase through run-rust (network, writable cache, audit), then an
  # offline build that sees the bundle at its path inside the project mount.
  "$RUN_RUST" -C "$REPO" fetch --locked

  env_file=$(make_env_file \
    "CANNERY_POSTGRES_BUNDLE=/workspace/${bundle#"$REPO"/}" \
    CARGO_NET_OFFLINE=true SQLX_OFFLINE=true)

  "$RUN" --project "$REPO" --cache-ro cargo --env-file "$env_file" \
    --memory 8g --cpus 4 --timeout 2h "$RUST_IMAGE" \
    cargo build --locked --offline --release -p cannery --bin cannery --features bundled-postgres
  rm -f "$env_file"
  env_file=

  mkdir -p "$STATE/bin"
  cp "$REPO/target/release/cannery" "$STATE/bin/cannery.new"
  mv "$STATE/bin/cannery.new" "$STATE/bin/cannery"
}

serve() {
  [[ -x "$STATE/bin/cannery" ]] || { echo "no binary: run $0 build" >&2; exit 1; }
  mkdir -p "$STATE/data"
  local settings=(
    CANNERY_DATABASE_PROVIDER=managed
    CANNERY_DATABASE_DATA_DIR=/data/db
    CANNERY_DATABASE_POSTGRES_CACHE_DIR=/data/cache
    CANNERY_STORAGE_BACKEND=local
    CANNERY_STORAGE_LOCAL_ROOT=/data/objects
    "CANNERY_SERVER_PUBLIC_BASE_URL=https://$NAME.$DEV_DOMAIN"
    "CANNERY_AUTH_BOOTSTRAP_ADMIN_EMAILS=$DEV_ADMIN_EMAILS"
  )
  if [[ -n "$DEV_OIDC_ISSUER" ]]; then
    local secret
    secret=$(oidc_client_secret)
    settings+=(
      "CANNERY_AUTH_OIDC_ISSUER=$DEV_OIDC_ISSUER"
      "CANNERY_AUTH_OIDC_CLIENT_ID=$DEV_OIDC_CLIENT_ID"
      "CANNERY_AUTH_OIDC_CLIENT_SECRET=$secret"
    )
  fi
  env_file=$(make_env_file "${settings[@]}")

  # initdb starts postgres through /bin/sh, so the image needs a shell; a
  # glibc base because the bundle links glibc.
  "$RUN" --serve "$NAME:8000" --env-file "$env_file" \
    --volume "$STATE/bin:/opt/cannery:ro" --volume "$STATE/data:/data" \
    --memory 1g --cpus 2 --timeout 8h "$SERVE_IMAGE" \
    /opt/cannery/cannery serve --host 0.0.0.0 --port 8000
}

case "${1:-all}" in
  build) build ;;
  serve) serve ;;
  all) build && serve ;;
  *) echo "usage: $0 [build|serve]" >&2; exit 64 ;;
esac
