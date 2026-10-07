#!/usr/bin/env bash
# Runs every test in ci/database-tests.txt (or those whose target matches the
# first argument) against its own migrated database.
#
#   CANNERY_TEST_DATABASE_URL=postgresql://postgres@host:5432/postgres \
#     ci/database-tests.sh MANIFEST.json [FILTER]
#
# MANIFEST.json is the output of `cargo test --no-run --message-format=json`
# for the crates listed; target/debug/cannery and
# target/debug/cannery-test-launcher must be built.
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest=${1:?usage: $0 MANIFEST.json [FILTER]}
filter=${2:-}
launcher=$repo/target/debug/cannery-test-launcher
server=$repo/target/debug/cannery
cd "$repo"
failed=()
ran=0
while read -r crate target args; do
  [[ -z "$crate" || "$crate" == \#* ]] && continue
  [[ -n "$filter" && "$target" != *"$filter"* ]] && continue
  kind='["test"]'
  [[ "$target" == cannery_* ]] && kind='["lib"]'
  executable=$(jq -ser --arg name "$target" --arg crate "/crates/$crate/Cargo.toml" --argjson kind "$kind" '
    [.[] | select(.reason == "compiler-artifact" and .profile.test == true
                  and .target.kind == $kind and .target.name == $name
                  and (.manifest_path | endswith($crate)) and .executable != null) | .executable]
    | unique | if length == 1 then .[0] else error("one test executable required for \($crate) \($name)") end' "$manifest")
  read -ra fields <<<"${args:-}"
  options=()
  extra=()
  seen_separator=false
  for field in "${fields[@]}"; do
    if [[ "$seen_separator" == false && "$field" == -- ]]; then
      seen_separator=true
    elif [[ "$seen_separator" == true ]]; then
      extra+=("$field")
    else
      options+=("$field")
    fi
  done
  echo "::group::$crate $target"
  ran=$((ran + 1))
  if ! "$launcher" database --server-binary "$server" "${options[@]}" -- \
      "$executable" --ignored --nocapture --test-threads=1 "${extra[@]}" </dev/null; then
    failed+=("$crate $target")
  fi
  echo "::endgroup::"
done < "$repo/ci/database-tests.txt"
(( ran > 0 )) || { echo "no database test matched" >&2; exit 1; }
if (( ${#failed[@]} )); then
  printf 'failed: %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "$ran database test targets passed"
