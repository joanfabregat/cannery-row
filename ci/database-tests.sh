#!/usr/bin/env bash
# Runs every test in ci/database-tests.txt (or those whose target matches the
# first argument) against its own migrated database.
#
#   CANNERY_TEST_DATABASE_URL=postgresql://postgres@host:5432/postgres \
#     [DATABASE_TEST_JOBS=N] ci/database-tests.sh MANIFEST.json [FILTER]
#
# MANIFEST.json is the output of `cargo test --no-run --message-format=json`
# for the crates listed; target/debug/cannery and
# target/debug/cannery-test-launcher must be built.
#
# DATABASE_TEST_JOBS (default 1) runs up to N test targets at once, each
# still against its own database with --test-threads=1; their output is
# printed as each one finishes. N > 1 needs bash 5.1 or later.
set -euo pipefail
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest=${1:?usage: $0 MANIFEST.json [FILTER]}
filter=${2:-}
parallel=${DATABASE_TEST_JOBS:-1}
launcher=$repo/target/debug/cannery-test-launcher
server=$repo/target/debug/cannery
cd "$repo"
if ! [[ "$parallel" =~ ^[1-9][0-9]*$ ]]; then
  echo "DATABASE_TEST_JOBS must be a positive integer" >&2
  exit 2
fi
if (( parallel > 1 )) && (( BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] < 501 )); then
  echo "DATABASE_TEST_JOBS > 1 needs bash 5.1 or later" >&2
  exit 2
fi
failed=()
ran=0
if (( parallel > 1 )); then
  logs=$(mktemp -d)
  trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$logs"' EXIT
  declare -A index=()
  labels=()
  started=()
  running=0
fi

# Waits for one background target, then prints its output in a group.
reap() {
  local pid status=0
  wait -n -p pid || status=$?
  local i=${index[$pid]}
  unset "index[$pid]"
  running=$((running - 1))
  echo "::group::${labels[i]} ($((SECONDS - started[i]))s)"
  cat "$logs/$i.log"
  echo "::endgroup::"
  if (( status != 0 )); then
    failed+=("${labels[i]}")
  fi
}

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
  for field in ${fields[@]+"${fields[@]}"}; do
    if [[ "$seen_separator" == false && "$field" == -- ]]; then
      seen_separator=true
    elif [[ "$seen_separator" == true ]]; then
      extra+=("$field")
    else
      options+=("$field")
    fi
  done
  test_command=("$launcher" database --server-binary "$server" ${options[@]+"${options[@]}"} --
    "$executable" --ignored --nocapture --test-threads=1 ${extra[@]+"${extra[@]}"})
  if (( parallel == 1 )); then
    echo "::group::$crate $target"
    ran=$((ran + 1))
    if ! "${test_command[@]}" </dev/null; then
      failed+=("$crate $target")
    fi
    echo "::endgroup::"
    continue
  fi
  while (( running >= parallel )); do reap; done
  labels[ran]="$crate $target"
  started[ran]=$SECONDS
  "${test_command[@]}" </dev/null >"$logs/$ran.log" 2>&1 &
  index[$!]=$ran
  running=$((running + 1))
  ran=$((ran + 1))
done < "$repo/ci/database-tests.txt"
if (( parallel > 1 )); then
  while (( running > 0 )); do reap; done
fi
(( ran > 0 )) || { echo "no database test matched" >&2; exit 1; }
if (( ${#failed[@]} )); then
  printf 'failed: %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "$ran database test targets passed"
