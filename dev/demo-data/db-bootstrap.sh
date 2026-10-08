#!/usr/bin/env bash
# The database half of dev/seed-demo.sh. It runs inside a postgres:17 image
# (for psql) next to a running managed instance, which it reaches over the
# Unix socket of the managed data directory; it never runs on the host.
#
#   /db           the instance's database.data_dir (run/ socket, password)
#   /demo         dev/demo-data, read-only
#   /opt/cannery  the instance's cannery binary, read-only
#
# Standard input holds the SQL of the short-lived demo sessions, written by
# dev/seed-demo.sh. Steps: create the demo users, open their sessions, then
# load each import bundle with `cannery import`.
set -euo pipefail

password=$(head -n 1 /db/password)
export PGPASSWORD=$password
psql_args=(-X -q -v ON_ERROR_STOP=1 -h /db/run -U cannery -d cannery)

psql "${psql_args[@]}" -f /demo/users.sql
psql "${psql_args[@]}" -f -

# `cannery import` takes the database as a URL; the managed server listens
# only on its socket, and its generated password is hexadecimal.
export CANNERY_DATABASE_PROVIDER=url
export CANNERY_DATABASE_URL="postgresql:///cannery?host=%2Fdb%2Frun&port=5432&user=cannery&password=$password"
for bundle in /demo/import/*/; do
  [[ -d "$bundle" ]] || continue
  slug=$(basename "$bundle")
  echo "importing $slug" >&2
  /opt/cannery/cannery import --bundle "$bundle" --project "$slug" \
    --science "/demo/projects/$slug/science.json"
done
