#!/usr/bin/env bash
# A test bench for driving a local Cannery Row instance with real AI agents
# (Claude Code, Codex) over MCP, one identity per agent session, through
# every phase: plan, run, verify, document, decide, with questions, steering
# and transcripts.
#
#   dev/local.sh serve        # in another terminal; leave it running
#   dev/bench.sh              # (or `dev/bench.sh up`) sets the bench up
#   dev/bench.sh show         # prints the paths and commands again
#   dev/bench.sh revoke       # revokes the bench's tokens, deletes its files
#
# `up` creates the bench project unless it exists (a fictional shop
# recommender, the science revision and producer of dev/demo-data's
# demo-recommender, its brief and three tracks in planning; a later run adds
# whatever an interrupted one left out), or uses an existing project named
# with --project as it is. It makes the fictional
# Bench Researcher and the users of --grant researchers of it, creates the
# service accounts the project's science revision needs, and mints one token
# per role:
#
#   researcher  the Bench Researcher's personal token: writes the brief,
#               plans and approves, answers and steers, writes up, decides
#   agent       service account bench-agent (kind agent): claims and runs units
#   verifier    with verify performer agent, service account bench-verifier
#               (kind agent): verifies the attempts it did not run; with
#               performer runner, the verifier service account the science
#               revision names, which claims runner verify jobs
#   documenter  service account bench-documenter (kind agent): writes units
#               up through document jobs; there is no documenter kind
#   decider     only with decide performer step: the decider service account
#               the science revision names, which claims decide jobs
#
# Each token is written, never printed, to a mode-0600 file under
# $STATE_ROOT/cannery-local/bench/<project>/, with per role an env file
# setting the role's token variable, a Claude Code MCP configuration and a
# Codex configuration snippet that name the variable and hold no secret.
# Running `up` again revokes the tokens of the previous run and mints new ones.
#
# Options (each also settable in the environment or dev/local.env):
#   --url URL        the instance (CANNERY_BENCH_URL, default https://$NAME.$DEV_DOMAIN,
#                    the URL dev/local.sh serves)
#   --data-dir DIR   its database.data_dir on the host (CANNERY_BENCH_DATA_DIR,
#                    default $STATE_ROOT/cannery-local/data/db)
#   --project SLUG   the project (CANNERY_BENCH_PROJECT, default bench)
#   --grant EMAILS   comma-separated emails of existing users (signed in once)
#                    to make researchers of the project (CANNERY_BENCH_GRANT,
#                    default DEV_ADMIN_EMAILS)
#   --days N         token lifetime in days (CANNERY_BENCH_DAYS, default 7)
#   --decide WHO     who decides in a project the bench creates: step (a
#                    decider service account, the default) or researcher
#                    (CANNERY_BENCH_DECIDE)
#
# Users exist only after an OIDC sign-in, and tokens are minted only from a
# browser session. As dev/seed-demo.sh does, a postgres:17 container
# (run-podman) reaches the instance's managed PostgreSQL through the socket in
# its data directory, inserts the two fictional users of
# dev/bench-users.sql (an .invalid issuer: nobody can sign in as them) and a
# session per user that expires within the hour. The script acts through
# those sessions over the REST API and signs them out when it ends. `revoke`
# opens an admin session the same way to revoke the service tokens.
#
# Needs curl, jq, yq (kislyuk or mikefarah), sha256sum, base64 and the
# container runner (RUN, run-podman by default).
source "$(dirname "$0")/common.sh"

DEMO=$REPO/dev/demo-data
TEMPLATE=$DEMO/projects/demo-recommender
NAME=${CANNERY_LOCAL_NAME:-cannery}
STATE=$STATE_ROOT/cannery-local
URL=${CANNERY_BENCH_URL:-https://$NAME.$DEV_DOMAIN}
DATA_DIR=${CANNERY_BENCH_DATA_DIR:-$STATE/data/db}
PROJECT=${CANNERY_BENCH_PROJECT:-bench}
GRANT=${CANNERY_BENCH_GRANT:-$DEV_ADMIN_EMAILS}
DAYS=${CANNERY_BENCH_DAYS:-7}
DECIDE=${CANNERY_BENCH_DECIDE:-step}
DB_IMAGE=docker.io/library/postgres:17-bookworm
ISSUER=https://bench-users.cannery-row.invalid
BENCH_TITLE="Bench: shop recommender"
USERS=(admin researcher)

usage() {
  echo "usage: $0 [up|show|revoke] [--url URL] [--data-dir DIR] [--project SLUG] [--grant EMAILS] [--days N] [--decide step|researcher]" >&2
  exit 64
}

COMMAND=up
case "${1-}" in
  up|show|revoke) COMMAND=$1; shift ;;
esac
while (($#)); do
  case "$1" in
    --url) URL=$2; shift 2 ;;
    --data-dir) DATA_DIR=$2; shift 2 ;;
    --project) PROJECT=$2; shift 2 ;;
    --grant) GRANT=$2; shift 2 ;;
    --days) DAYS=$2; shift 2 ;;
    --decide) DECIDE=$2; shift 2 ;;
    -h|--help) sed -n '2,/^source/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) usage ;;
  esac
done
URL=${URL%/}
[[ "$PROJECT" =~ ^[a-z0-9][a-z0-9-]*$ ]] || { echo "bad project slug: $PROJECT" >&2; exit 64; }
[[ "$DAYS" =~ ^[1-9][0-9]*$ ]] || { echo "--days takes a positive number" >&2; exit 64; }
[[ "$DECIDE" == step || "$DECIDE" == researcher ]] || usage
BASE=/api/projects/$PROJECT
BENCH=$STATE/bench/$PROJECT
# The record of what the bench minted: token ids and roles, no secret.
RECORD=$BENCH/bench.json

log() { printf 'bench: %s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

for tool in curl jq yq sha256sum base64; do
  command -v "$tool" >/dev/null || die "$tool is required"
done

# Session secrets and the API's answers live in a private directory in /tmp
# (RAM), never on a command line: curl reads each header set from a file.
SECRETS=$(umask 077 && mktemp -d /tmp/cannery-bench.XXXXXX)
cleanup() {
  local status=$?
  if [[ -f "$SECRETS/sessions-open" ]]; then
    for user in "${USERS[@]}"; do
      [[ -f "$SECRETS/user-$user.h" ]] || continue
      curl -sS -o /dev/null -X POST -H "@$SECRETS/user-$user.h" "$URL/auth/logout" || true
    done
  fi
  rm -rf "$SECRETS"
  exit "$status"
}
trap cleanup EXIT

yaml_json() {
  if yq --version 2>&1 | grep -qi mikefarah; then yq -o=json '.' "$1"; else yq '.' "$1"; fi
}

# ---------------------------------------------------------------- HTTP

# api_status USER METHOD PATH [BODY]: prints the HTTP status and leaves the
# answer in $SECRETS/body.
api_status() {
  local user=$1 method=$2 path=$3 body=${4-}
  local args=(-sS -o "$SECRETS/body" -w '%{http_code}' -X "$method" -H "@$SECRETS/user-$user.h")
  if [[ -n "$body" ]]; then
    args+=(-H 'Content-Type: application/json' --data-binary @-)
  fi
  printf '%s' "$body" | curl "${args[@]}" "$URL$path" || die "$method $path: request failed"
}

# api USER METHOD PATH [BODY]: prints the JSON answer; any status but 2xx
# stops the script with the API's error object.
api() {
  local user=$1 method=$2 path=$3 code
  code=$(api_status "$@")
  if [[ "$code" != 2* ]]; then
    log "$method $path answered $code as $user:"
    jq -c '.error // .' "$SECRETS/body" >&2 2>/dev/null || head -c 2000 "$SECRETS/body" >&2
    exit 1
  fi
  cat "$SECRETS/body"
}

# status_of USER PATH: the HTTP status of a GET.
status_of() {
  curl -sS -o /dev/null -w '%{http_code}' -H "@$SECRETS/user-$1.h" "$URL$2"
}

# open_sessions USER...: the fictional users, and a session for each that
# expires within the hour, written through the managed PostgreSQL's socket.
open_sessions() {
  local sql=$SECRETS/sessions.sql secret csrf hash user
  [[ -S "$DATA_DIR/run/.s.PGSQL.5432" ]] ||
    die "no managed PostgreSQL socket in $DATA_DIR/run: start the instance first (dev/local.sh serve) or pass --data-dir"
  : >"$sql"
  for user in "$@"; do
    secret="cr_ses_$(head -c 32 /dev/urandom | base64 -w0 | tr '+/' '-_' | tr -d '=')"
    csrf=$(head -c 24 /dev/urandom | base64 -w0 | tr '+/' '-_' | tr -d '=')
    hash=$(printf '%s' "$secret" | sha256sum | cut -d' ' -f1)
    printf 'Cookie: cr_session=%s\nX-CSRF-Token: %s\n' "$secret" "$csrf" >"$SECRETS/user-$user.h"
    printf "INSERT INTO sessions (secret_hash, user_id, csrf_token, expires_at) SELECT '\\\\x%s'::bytea, id, '%s', now() + interval '1 hour' FROM users WHERE issuer = '%s' AND subject = '%s';\n" \
      "$hash" "$csrf" "$ISSUER" "$user" >>"$sql"
  done
  : >"$SECRETS/sessions-open"
  log "opening sessions for the bench users"
  # dev/demo-data/db-bootstrap.sh with the bench users in place of the demo
  # users and no import bundle. The SQL holds only digests of the secrets.
  "$RUN" --volume "$DATA_DIR:/db" \
    --volume "$DEMO/db-bootstrap.sh:/demo/db-bootstrap.sh:ro" \
    --volume "$REPO/dev/bench-users.sql:/demo/users.sql:ro" \
    --memory 1g --timeout 5m "$DB_IMAGE" bash /demo/db-bootstrap.sh <"$sql"
  rm -f "$sql"
  api admin GET /api/me >/dev/null
}

user_id() {
  local found
  found=$(api admin GET "/api/users?email=$(jq -rn --arg e "$1" '$e|@uri')" | jq -r '.items[0].id // empty')
  [[ -n "$found" ]] && printf '%s' "$found"
}

# ---------------------------------------------------------------- project

# setup_project: the project, created unless it exists. The bench's own
# project (titled BENCH_TITLE) gets whatever it still lacks, so a setup cut
# short is finished by the next run: the demo recommender's science revision
# (its decider renamed, or none with --decide researcher), producer, tracks
# and brief. Any other project is used as it is.
setup_project() {
  local spec=$SECRETS/template.json title track slug
  yaml_json "$TEMPLATE/project.yaml" >"$spec"
  # The admin is no member, so it learns whether the project exists by
  # creating it; the researcher, a member from then on, reads it.
  if [[ "$(api_status admin POST /api/projects "$(jq -c --arg s "$PROJECT" --arg t "$BENCH_TITLE" '{slug: $s, title: $t,
      description: "A fictional shop recommender for driving Cannery Row with AI agents (dev/bench.sh). Demo data: no real research.",
      tracks: [.tracks[0] | {slug, title, description}]}' "$spec")")" == 2* ]]; then
    log "created project $PROJECT"
  elif [[ "$(jq -r .error.code "$SECRETS/body")" != conflict ]]; then
    die "could not create project $PROJECT: $(jq -c '.error // .' "$SECRETS/body")"
  fi
  grant
  title=$(api researcher GET "$BASE" | jq -r .title)
  if [[ "$title" != "$BENCH_TITLE" ]]; then
    log "using the existing project $PROJECT as it is"
    [[ "$(status_of researcher "$BASE/config/science/latest")" == 200 ]] ||
      die "$PROJECT has no science revision yet: register one first"
    return
  fi
  if [[ "$(status_of researcher "$BASE/config/science/latest")" == 404 ]]; then
    api admin POST "$BASE/config/science" "$(jq -c --arg decide "$DECIDE" '
      if $decide == "step" then .decide = {performer: "step", decider: {id: "bench-decider", revision: "bench-decider-1"}}
      else del(.decide) end' "$TEMPLATE/science.json")" >/dev/null
  fi
  if [[ "$(status_of researcher "$BASE/producers/$(jq -r .metadata.name "$TEMPLATE/producer.json")/1")" == 404 ]]; then
    api admin POST "$BASE/producers" "$(cat "$TEMPLATE/producer.json")" >/dev/null
  fi
  while read -r track; do
    slug=$(jq -r .slug <<<"$track")
    [[ "$(status_of researcher "$BASE/tracks/$slug")" != 404 ]] || api researcher POST "$BASE/tracks" "$track" >/dev/null
  done < <(jq -c '.tracks[] | {slug, title, description}' "$spec")
  if [[ "$(status_of researcher "$BASE/brief")" == 404 ]]; then
    api researcher POST "$BASE/brief" "$(jq -c '{document: .brief.document, expected_revision: 0}' "$spec")" >/dev/null
  fi
}

grant() {
  local email id
  api admin PUT "$BASE/members/$(user_id bench.researcher@example.com)" '{"role":"researcher"}' >/dev/null
  IFS=, read -ra emails <<<"$GRANT"
  for email in "${emails[@]}"; do
    email=${email// /}
    [[ -n "$email" ]] || continue
    if id=$(user_id "$email"); then
      api admin PUT "$BASE/members/$id" '{"role":"researcher"}' >/dev/null
      log "$email is a researcher of $PROJECT"
    else
      log "no user $email yet (sign in once, then run this again); skipped"
    fi
  done
}

# roles: one line per service role, "ROLE KIND ACCOUNT", from the project's
# current science revision.
roles() {
  local science
  science=$(api researcher GET "$BASE/config/science/latest" | jq -c .content)
  echo "agent agent bench-agent"
  if [[ "$(jq -r .verify.performer <<<"$science")" == runner ]]; then
    echo "verifier verifier $(jq -r .verify.verifier.id <<<"$science")"
  else
    echo "verifier agent bench-verifier"
  fi
  echo "documenter agent bench-documenter"
  if [[ "$(jq -r '.decide.performer // "researcher"' <<<"$science")" == step ]]; then
    echo "decider decider $(jq -r .decide.decider.id <<<"$science")"
  fi
}

# account KIND NAME ROLE: the service account, created unless it exists.
account() {
  local kind=$1 name=$2 role=$3 existing
  existing=$(api admin GET "$BASE/service-accounts?limit=200" | jq -c --arg n "$name" '[.items[] | select(.name == $n)][0] // empty')
  if [[ -z "$existing" ]]; then
    api admin POST "$BASE/service-accounts" "$(jq -nc --arg k "$kind" --arg n "$name" --arg r "$role" \
      '{kind: $k, name: $n, description: "The \($r) of the dev/bench.sh test bench."}')" >/dev/null
    log "created service account $name ($kind)"
    return
  fi
  [[ "$(jq -r .kind <<<"$existing")" == "$kind" ]] ||
    die "service account $name exists with kind $(jq -r .kind <<<"$existing"), not $kind"
  [[ "$(jq -r .disabled_at <<<"$existing")" == null ]] || die "service account $name is disabled"
}

# ---------------------------------------------------------------- files

var_of() { printf 'CANNERY_BENCH_%s_TOKEN' "${1^^}"; }

# write_role ROLE TOKEN_JSON: the token and the role's client files.
write_role() {
  local role=$1 token=$2 var
  var=$(var_of "$role")
  (
    umask 077
    jq -r .token <<<"$token" >"$BENCH/$role.token"
    printf '%s=%s\n' "$var" "$(jq -r .token <<<"$token")" >"$BENCH/$role.env"
  )
  jq -n --arg url "$URL/mcp" --arg var "$var" \
    '{mcpServers: {"cannery-row": {type: "http", url: $url, headers: {Authorization: "Bearer ${\($var)}"}}}}' \
    >"$BENCH/$role.mcp.json"
  printf '[mcp_servers.cannery_row]\nurl = "%s"\nbearer_token_env_var = "%s"\n' "$URL/mcp" "$var" >"$BENCH/$role.codex.toml"
}

# revoke_recorded: revokes every token of the record (the admin's session
# may revoke any token) and deletes the bench's files.
revoke_recorded() {
  local id role code failed=0
  while read -r role id; do
    code=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "@$SECRETS/user-admin.h" "$URL/api/tokens/$id")
    if [[ "$code" == 2* || "$code" == 404 ]]; then
      log "revoked the $role token"
    else
      log "could not revoke the $role token $id ($code)"
      failed=1
    fi
  done < <(jq -r '.tokens[] | "\(.role) \(.id)"' "$RECORD")
  ((failed == 0)) || die "kept $BENCH: revoke again once the instance answers"
  rm -rf "$BENCH"
}

show() {
  [[ -f "$RECORD" ]] || die "no bench for $PROJECT: run $0 up"
  local role var
  printf 'Project %s at %s, MCP %s\n' "$PROJECT" "$(jq -r .url "$RECORD")" "$(jq -r .url "$RECORD")/mcp"
  printf 'Files in %s; tokens expire at %s.\n' "$BENCH" "$(jq -r '.tokens[0].expires_at' "$RECORD")"
  printf 'Start one agent session per role, in a working directory of your choice:\n'
  while read -r role; do
    var=$(var_of "$role")
    printf '\n%s (%s)\n' "$role" "$(jq -r --arg r "$role" '.tokens[] | select(.role == $r) | .identity' "$RECORD")"
    printf '  claude: (set -a && . %s && exec claude --strict-mcp-config --mcp-config %s)\n' \
      "$BENCH/$role.env" "$BENCH/$role.mcp.json"
    printf "  codex:  (set -a && . %s && exec codex -c 'mcp_servers.cannery_row.url=\"%s\"' -c 'mcp_servers.cannery_row.bearer_token_env_var=\"%s\"')\n" \
      "$BENCH/$role.env" "$(jq -r .url "$RECORD")/mcp" "$var"
  done < <(jq -r '.tokens[].role' "$RECORD")
  printf '\nRevoke: %s revoke --project %s\n' "$0" "$PROJECT"
}

# ---------------------------------------------------------------- commands

up() {
  curl -fsS -o /dev/null "$URL/api/health" || die "$URL/api/health does not answer: is the instance running?"
  open_sessions "${USERS[@]}"
  if [[ -f "$RECORD" ]]; then
    log "revoking the tokens of the previous run"
    revoke_recorded
  fi
  setup_project

  (umask 077 && mkdir -p "$BENCH")
  chmod 700 "$BENCH" "$STATE/bench"
  local tokens=() token role kind name body services
  services=$(roles)
  body=$(jq -nc --argjson d "$DAYS" '{name: "bench", expires_in_days: $d, scopes: ["read", "write"]}')
  token=$(api researcher POST /api/tokens "$body")
  write_role researcher "$token"
  tokens+=("$(jq -c '{role: "researcher", identity: "Bench Researcher, personal token", id, expires_at}' <<<"$token")")
  while read -r role kind name; do
    account "$kind" "$name" "$role"
    token=$(api admin POST "$BASE/service-accounts/$name/tokens" "$body")
    write_role "$role" "$token"
    tokens+=("$(jq -c --arg r "$role" --arg i "service account $name, kind $kind" '{role: $r, identity: $i, id, expires_at}' <<<"$token")")
  done <<<"$services"
  printf '%s\n' "${tokens[@]}" | jq -s --arg url "$URL" --arg p "$PROJECT" '{url: $url, project: $p, tokens: .}' >"$RECORD"
  log "minted ${#tokens[@]} tokens"
  show
}

revoke() {
  [[ -f "$RECORD" ]] || die "no bench for $PROJECT in $BENCH"
  URL=$(jq -r .url "$RECORD")
  open_sessions admin
  revoke_recorded
  log "deleted $BENCH"
}

case "$COMMAND" in
  up) up ;;
  show) show ;;
  revoke) revoke ;;
esac
