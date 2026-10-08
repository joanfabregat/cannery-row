#!/usr/bin/env bash
# Builds the managed PostgreSQL bundle on a CI runner, in two phases:
#   1. fetch (network): the pinned source tarball, checked against PG_SHA256
#      in dev/postgres-bundle-build.sh, and on Linux the bison, flex and zstd
#      packages, which apt verifies against the signed Debian archive;
#   2. build: dev/postgres-bundle-build.sh, on Linux offline in the
#      buildpack-deps:bookworm image pinned by digest for the runner's
#      architecture (same pins as dev/build-postgres-bundle.sh), on macOS
#      natively with the Xcode command line tools and the runner image's GNU
#      tar, zstd and xz (nothing is installed).
#
#   ci/postgres-bundle.sh WORK_DIR
#
# The bundle and its .sha256, .dynamic.txt and .sizes.txt land in
# WORK_DIR/out. The rust workflow caches that directory under a key that
# hashes this script and dev/postgres-bundle-build.sh, so a change to either
# rebuilds it. Keep it compatible with bash 3.2 (macOS /bin/bash).
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=${1:?usage: $0 WORK_DIR}
build_script=$repo/dev/postgres-bundle-build.sh
version=$(sed -n 's/^PG_VERSION=//p' "$build_script")
digest=$(sed -n 's/^PG_SHA256=//p' "$build_script")
mkdir -p "$work/debs" "$work/build" "$work/out"
tarball=$work/postgresql.tar.bz2

curl -sfL -o "$tarball" \
  "https://ftp.postgresql.org/pub/source/v$version/postgresql-$version.tar.bz2"

case $(uname -s) in
  Linux)
    echo "$digest  $tarball" | sha256sum -c -
    case $(uname -m) in
      x86_64) image=docker.io/library/buildpack-deps:bookworm@sha256:672aaedcfec98774308e902ae592697bc34b05d58f62104015cf3511f58e318a ;;
      aarch64) image=docker.io/library/buildpack-deps:bookworm@sha256:713468e310395862392894a4dea5996eb8f6f76498c74dcbd37613e0f740096f ;;
      *) echo "unsupported architecture $(uname -m)" >&2 && exit 1 ;;
    esac
    # apt verifies the packages against the signed Debian archive.
    docker run --rm -v "$work/debs:/debs" -w /debs "$image" \
      /bin/sh -c 'apt-get update -qq && apt-get download -qq bison flex zstd'
    docker run --rm --user "$(id -u):$(id -g)" --network none --cap-drop ALL \
      --security-opt no-new-privileges --read-only \
      --tmpfs /tmp:rw,exec,nosuid,nodev,size=512m --memory 4g --cpus "$(nproc)" \
      -v "$work:/pg" -v "$build_script:/build.sh:ro" \
      "$image" /bin/sh -c '
        set -eu
        for deb in /pg/debs/*.deb; do dpkg -x "$deb" /tmp/tools; done
        export PATH=/tmp/tools/usr/bin:$PATH BISON_PKGDATADIR=/tmp/tools/usr/share/bison
        bash /build.sh /pg/postgresql.tar.bz2 /pg/out /pg/build/work'
    ;;
  Darwin)
    echo "$digest  $tarball" | shasum -a 256 -c -
    : "${MACOSX_DEPLOYMENT_TARGET:?set MACOSX_DEPLOYMENT_TARGET}"
    # Xcode command line tools (bison 2.3 is enough for PostgreSQL 17) and
    # the runner image's GNU tar, zstd and xz.
    for tool in cc make perl bison flex otool install_name_tool codesign gtar zstd xz; do
      command -v "$tool"
    done
    bison --version | head -n 1
    bash "$build_script" "$tarball" "$work/out" "$work/build/work"
    ;;
  *) echo "unsupported OS $(uname -s)" >&2 && exit 1 ;;
esac
