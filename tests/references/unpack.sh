#!/usr/bin/env bash
# Unpacks the frozen reference outputs into the paths the tests read
# (crates/*/tests/fixtures/**). The files are git-ignored; run this
# once after checkout, and again whenever references.tar.zst changes.
# Needs tar and zstd only; it executes nothing from the archive.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
(cd "$here" && sha256sum -c --quiet references.tar.zst.sha256)
zstd -dc "$here/references.tar.zst" |
  tar -x -C "$repo" --no-same-owner --no-same-permissions --no-overwrite-dir
