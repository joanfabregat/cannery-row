#!/usr/bin/env bash
# Builds the relocatable PostgreSQL bundle that `cannery` embeds with the
# `bundled-postgres` feature. On Linux it runs INSIDE a build container
# (Debian bookworm with gcc, make, perl, bison, flex, zstd and xz on PATH);
# locally it is started by dev/build-postgres-bundle.sh, in CI by the rust
# workflow. On macOS it runs on the host with the Xcode command line tools
# (clang, make, perl, bison 2.3, flex, otool, install_name_tool, codesign)
# plus GNU tar as `gtar`, zstd and xz; set MACOSX_DEPLOYMENT_TARGET first.
# Keep it compatible with bash 3.2 (macOS /bin/bash).
#
# Usage: postgres-bundle-build.sh SOURCE_TARBALL OUT_DIR [WORK_DIR]
#
# The tarball must already be verified against PG_SHA256 below; this script
# checks it again. Output: OUT_DIR/postgresql-<version>-<os>-<arch>.tar.zst
# (os linux or macos, arch x86_64 or aarch64) and its .sha256, plus a
# .tar.xz for size comparison.
set -euo pipefail

PG_VERSION=17.11
PG_SHA256=dd27f2b3c59e73ed14aa3324901242bf69a032a6347805f274e6260322d42979

source_tarball=$1
out_dir=$2
work_dir=${3:-/tmp/postgres-bundle}
# A prefix containing "postgres" keeps lib/ and share/ flat (no
# "postgresql" subdirectory). Nothing is ever installed there: the tree is
# relocatable and the binaries find lib/ and share/ relative to bin/.
prefix=/opt/postgresql
case $(uname -s) in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "unsupported OS $(uname -s)" >&2 && exit 1 ;;
esac
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2 && exit 1 ;;
esac
name=postgresql-$PG_VERSION-$os-$arch

if [ "$os" = linux ]; then
  sha256() { sha256sum "$@"; }
  jobs=$(nproc)
  tar=tar
else
  sha256() { shasum -a 256 "$@"; }
  jobs=$(sysctl -n hw.ncpu)
  tar=gtar
fi
# The archive options below (--sort, --hard-dereference) are GNU tar's.
if ! "$tar" --version 2>/dev/null | grep -q 'GNU tar'; then
  echo "GNU tar is required as $tar" >&2
  exit 1
fi
byte_size() { wc -c <"$1" | tr -d ' '; }

echo "$PG_SHA256  $source_tarball" | sha256 -c -
rm -rf "$work_dir"
mkdir -p "$work_dir" "$out_dir"
tar -xjf "$source_tarball" -C "$work_dir"
cd "$work_dir/postgresql-$PG_VERSION"

# Runtime dependencies are the C library only (glibc, or libSystem on
# macOS): no OpenSSL, ICU, readline, zlib, compression, XML, Kerberos, LDAP,
# PAM, systemd, LLVM or language handlers beyond the built-in PL/pgSQL.
# Since PostgreSQL 17 the release tarball ships no generated parsers, so
# bison (2.3 or later) and flex (2.5.35 or later) are needed everywhere.
platform_flags=()
if [ "$os" = macos ]; then
  # Room for the relative install names written below.
  platform_flags=(LDFLAGS=-Wl,-headerpad_max_install_names)
fi
./configure --prefix="$prefix" --without-openssl --without-readline \
  --without-icu --without-zlib --without-zstd --without-lz4 --without-libxml \
  --without-libxslt --without-gssapi --without-ldap --without-pam \
  --without-systemd --without-selinux --without-perl \
  --without-python --without-tcl --without-llvm --disable-nls \
  --disable-debug CFLAGS=-O2 ${platform_flags[@]+"${platform_flags[@]}"} \
  >"$work_dir/configure.log"

# libpq consumers find it through a relative RUNPATH, never an absolute one
# (Linux; on macOS the install names are rewritten after the install).
rpath='$$ORIGIN/../lib'
make -s -j"$jobs" rpathdir="$rpath"
make -s -j"$jobs" -C contrib/pg_trgm rpathdir="$rpath"
stage=$work_dir/stage
make -s install-strip DESTDIR="$stage" rpathdir="$rpath"
make -s -C contrib/pg_trgm install-strip DESTDIR="$stage" rpathdir="$rpath"

root=$stage$prefix
cd "$root"
# Keep the server, initdb, pg_ctl and the dump tools; drop everything else.
for binary in bin/*; do
  case ${binary#bin/} in
    postgres | initdb | pg_ctl | pg_dump | pg_restore | pg_isready) ;;
    *) rm "$binary" ;;
  esac
done
rm -rf include share/doc share/man lib/pgxs lib/pkgconfig
rm -f lib/*.a lib/libecpg* lib/libpgtypes*
# Only libpq under its SONAME (install name on macOS) is needed, as a
# regular file: the bundle contains no symbolic links, which keeps
# extraction simple and safe.
if [ "$os" = linux ]; then
  cp --remove-destination "$(readlink -f lib/libpq.so.5)" lib/libpq.tmp
  rm -f lib/libpq.so lib/libpq.so.5*
  mv lib/libpq.tmp lib/libpq.so.5
else
  cp -L lib/libpq.5.dylib lib/libpq.tmp
  rm -f lib/libpq.dylib lib/libpq.5*.dylib
  mv lib/libpq.tmp lib/libpq.5.dylib
  # PostgreSQL links its libraries by absolute install name. Point every
  # reference into the prefix at the file relative to the loader instead
  # (@loader_path is bin/ for the programs, lib/ for loadable modules),
  # then re-sign ad hoc: arm64 macOS kills a binary whose signature the
  # edit invalidated.
  install_name_tool -id @rpath/libpq.5.dylib lib/libpq.5.dylib
  for file in bin/* lib/*.dylib; do
    case $file in
      bin/*) loader=@loader_path/../lib ;;
      *) loader=@loader_path ;;
    esac
    otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r dependency; do
      case $dependency in
        "$prefix"/lib/*)
          install_name_tool -change "$dependency" "$loader/${dependency##*/}" "$file"
          ;;
      esac
    done
    codesign --force --sign - "$file"
  done
fi
find share/extension -type f ! -name 'plpgsql*' ! -name 'pg_trgm*' -delete
if [ -n "$(find . -type l -o ! -type f ! -type d)" ]; then
  echo "bundle contains entries other than files and directories" >&2
  exit 1
fi
printf '%s\n' "$PG_VERSION" >VERSION

if [ "$os" = linux ]; then
  for binary in bin/* lib/*.so*; do
    readelf -d "$binary" | awk -v f="$binary" '/NEEDED|RUNPATH|RPATH/ {print f": "$0}'
  done >"$out_dir/$name.dynamic.txt"
else
  # "file: library" per linked library (and a dylib's own install name),
  # "file: rpath <path>" per LC_RPATH.
  for binary in bin/* lib/*.dylib; do
    otool -L "$binary" | tail -n +2 | awk -v f="$binary" '{print f": "$1}'
    otool -l "$binary" | awk -v f="$binary" '/cmd LC_RPATH/ {r = 1} r && $1 == "path" {print f": rpath "$2; r = 0}'
  done >"$out_dir/$name.dynamic.txt"
fi

# Deterministic archive: sorted names, fixed times and owners. Hard links
# (the time zone database has many) are stored as regular files.
"$tar" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner --format=gnu \
  --hard-dereference \
  -cf "$work_dir/$name.tar" VERSION bin lib share
zstd -q -19 -f "$work_dir/$name.tar" -o "$out_dir/$name.tar.zst"
xz -9e -T1 -c "$work_dir/$name.tar" >"$out_dir/$name.tar.xz"
(cd "$out_dir" && sha256 "$name.tar.zst" >"$name.tar.zst.sha256")
du -sk "$root" | awk '{print "unpacked_kib", $1}' >"$out_dir/$name.sizes.txt"
printf 'tar_bytes %s\n' "$(byte_size "$work_dir/$name.tar")" >>"$out_dir/$name.sizes.txt"
printf 'zst_bytes %s\n' "$(byte_size "$out_dir/$name.tar.zst")" >>"$out_dir/$name.sizes.txt"
printf 'xz_bytes %s\n' "$(byte_size "$out_dir/$name.tar.xz")" >>"$out_dir/$name.sizes.txt"
cat "$out_dir/$name.sizes.txt"
