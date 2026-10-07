#!/usr/bin/env bash
# Builds the PostgreSQL bundle for the `bundled-postgres` feature with run-podman,
# through ~/bin/run-podman, in two phases:
#   1. fetch (network): the upstream source tarball, checked against its
#      pinned SHA-256 on the host, and the bison, flex and zstd packages,
#      which apt verifies against the signed Debian archive;
#   2. build (offline): dev/postgres-bundle-build.sh in the same image.
#
# Usage: dev/build-postgres-bundle.sh OUT_DIR
# OUT_DIR must be under a directory the runner can mount (WORK_ROOT or
# STATE_ROOT, see dev/common.sh). Build with the result:
#   CANNERY_POSTGRES_BUNDLE=OUT_DIR/postgresql-<version>-linux-<arch>.tar.zst \
#     cargo build -p cannery --features bundled-postgres
# The macOS bundle is built natively on a Mac (see the rust workflow).
source "$(dirname "$0")/common.sh"

# Per-architecture manifests of buildpack-deps:bookworm; the same pins are
# in the managed-postgres job of .github/workflows/rust.yml.
case $(uname -m) in
  x86_64) BUILD_IMAGE=docker.io/library/buildpack-deps:bookworm@sha256:672aaedcfec98774308e902ae592697bc34b05d58f62104015cf3511f58e318a ;;
  aarch64) BUILD_IMAGE=docker.io/library/buildpack-deps:bookworm@sha256:713468e310395862392894a4dea5996eb8f6f76498c74dcbd37613e0f740096f ;;
  *) echo "unsupported architecture $(uname -m)" >&2 && exit 1 ;;
esac
PG_VERSION=$(sed -n 's/^PG_VERSION=//p' "$REPO/dev/postgres-bundle-build.sh")
PG_SHA256=$(sed -n 's/^PG_SHA256=//p' "$REPO/dev/postgres-bundle-build.sh")

out_dir=$(realpath -m "${1:?usage: $0 OUT_DIR}")
mkdir -p "$out_dir"
work=$(mktemp -d -p "$WORK_ROOT" pg-bundle-XXXXXXXX)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/debs" "$work/build"

tarball=$work/postgresql-$PG_VERSION.tar.bz2
curl -sfL -o "$tarball" \
  "https://ftp.postgresql.org/pub/source/v$PG_VERSION/postgresql-$PG_VERSION.tar.bz2"
echo "$PG_SHA256  $tarball" | sha256sum -c -

# The root filesystem is read-only, so apt keeps its lists in /tmp and only
# downloads; the packages are unpacked with dpkg -x in the build phase.
"$RUN" --network --volume "$work/debs:/debs" --memory 1g --timeout 15m \
  "$BUILD_IMAGE" /bin/sh -c '
    set -eu
    mkdir -p /tmp/apt/lists/partial /tmp/apt/cache/archives/partial
    o="-o Dir::State::Lists=/tmp/apt/lists -o Dir::Cache=/tmp/apt/cache -o Debug::NoLocking=true"
    apt-get $o -qq update
    cd /debs && apt-get $o -qq download bison flex zstd
  '

"$RUN" --volume "$tarball:/src/postgresql.tar.bz2:ro" \
  --volume "$work/debs:/debs:ro" \
  --volume "$REPO/dev/postgres-bundle-build.sh:/src/build.sh:ro" \
  --volume "$work/build:/build" --volume "$out_dir:/out" \
  --memory 4g --cpus 4 --timeout 1h "$BUILD_IMAGE" /bin/sh -c '
    set -eu
    for deb in /debs/*.deb; do dpkg -x "$deb" /tmp/tools; done
    export PATH=/tmp/tools/usr/bin:$PATH BISON_PKGDATADIR=/tmp/tools/usr/share/bison
    bash /src/build.sh /src/postgresql.tar.bz2 /out /build/work
  '
