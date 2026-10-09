#!/usr/bin/env bash
# Loads fictional demo projects (dev/demo-data) into a running Cannery Row
# instance with a managed database, so the web app can be developed against
# realistic content. No real research data, people or companies: every user
# is on example.com and every dataset, model and number is made up.
#
#   dev/local.sh serve        # in another terminal; leave it running
#   dev/seed-demo.sh          # loads the demo into that instance
#
# Options (each also settable in the environment or dev/local.env):
#   --url URL        the instance (CANNERY_DEMO_URL, default https://$NAME.$DEV_DOMAIN,
#                    the URL dev/local.sh serves)
#   --data-dir DIR   its database.data_dir on the host (CANNERY_DEMO_DATA_DIR,
#                    default $STATE_ROOT/cannery-local/data/db)
#   --bin-dir DIR    the directory of its cannery binary, used for
#                    `cannery import` (CANNERY_DEMO_BIN_DIR, default
#                    $STATE_ROOT/cannery-local/bin)
#   --grant EMAILS   comma-separated emails of existing users (signed in once)
#                    to make researchers of every demo project
#                    (CANNERY_DEMO_GRANT, default DEV_ADMIN_EMAILS)
#
# The instance must be running: everything but the users goes through the
# REST API, with the same validation as live work. Two things cannot:
#
# - Users exist only after an OIDC sign-in, and tokens are minted only from a
#   browser session. A postgres:17 container (run-podman) reaches the
#   instance's managed PostgreSQL through the socket in its data directory,
#   inserts the fictional users of dev/demo-data/users.sql (an .invalid
#   issuer: nobody can sign in as them) and one session per user that
#   expires within two hours. The script then acts as those users through
#   their sessions, and mints the projects' service tokens through the API.
#   At the end it revokes the tokens and signs the sessions out.
# - The forecast project's earlier history is a reviewed import bundle
#   (dev/demo-data/import/demo-forecast), loaded with `cannery import` by the
#   instance's own binary in the same container, over the same socket.
#
# Each project is created with its first track, then gets its other tracks and
# its brief, written by the researcher project.yaml names under brief.by.
# Tracks start in planning. A hypothesis the scenario approves in one step is
# a unit of its track's plan: the track's planner (plan.by, default
# brief.by) starts a plan revision, sets the approach (plan.approach, default
# the description) and adds the units, with the rationale as the unit brief and
# a context item per derived_from relation, then checks and submits it, and the
# reviewer (plan.review.by, default brief.by) approves it, which makes the
# hypotheses queued and the track active. A track with plan: false opts out.
# A unit naming a hypothesis that does not exist yet waits for a later
# revision of the plan.
#
# The REST part drives each hypothesis of dev/demo-data/projects/*/project.yaml
# through the real lifecycle: drafts by the demo agent or a researcher, draft
# reviews (approve, request_revision then revise, decline), claims, uploads,
# claimed result sheets, test jobs completed as the project's tester, verdicts
# as its evaluator, human decisions, failure reviews, comments and track
# transitions. There is no runner: the script plays the agent, the tester and
# the evaluator itself. The attempt left running keeps its lease for
# leases.ttl_seconds (15 minutes by default), then the sweep fails it with
# lease_expired and opens a failure review, as for a crashed agent.
#
# Loading is not repeatable: the script refuses an instance that already has
# a demo project. To start over, stop the instance, delete its data
# directory ($STATE_ROOT/cannery-local/data) and serve it again.
#
# Needs curl, jq, yq (kislyuk or mikefarah), sha256sum, base64 and the
# container runner (RUN, run-podman by default).
source "$(dirname "$0")/common.sh"

DEMO=$REPO/dev/demo-data
NAME=${CANNERY_LOCAL_NAME:-cannery}
STATE=$STATE_ROOT/cannery-local
URL=${CANNERY_DEMO_URL:-https://$NAME.$DEV_DOMAIN}
DATA_DIR=${CANNERY_DEMO_DATA_DIR:-$STATE/data/db}
BIN_DIR=${CANNERY_DEMO_BIN_DIR:-$STATE/bin}
GRANT=${CANNERY_DEMO_GRANT:-$DEV_ADMIN_EMAILS}
DB_IMAGE=docker.io/library/postgres:17-bookworm
ISSUER=https://demo-users.cannery-row.invalid
PROJECTS=${CANNERY_DEMO_PROJECTS:-demo-sentiment demo-recommender demo-forecast}

while (($#)); do
  case "$1" in
    --url) URL=$2; shift 2 ;;
    --data-dir) DATA_DIR=$2; shift 2 ;;
    --bin-dir) BIN_DIR=$2; shift 2 ;;
    --grant) GRANT=$2; shift 2 ;;
    -h|--help) sed -n '2,/^source/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "usage: $0 [--url URL] [--data-dir DIR] [--bin-dir DIR] [--grant EMAILS]" >&2; exit 64 ;;
  esac
done
URL=${URL%/}

# The fictional users, by the key project.yaml uses.
declare -A EMAIL=(
  [ada]=ada.marlowe@example.com
  [ben]=ben.okoro@example.com
  [chloe]=chloe.varga@example.com
  [dev]=dev.raman@example.com
  [eli]=eli.novak@example.com
)
USERS=(ada ben chloe dev eli)
ADMIN=ada

log() { printf 'seed-demo: %s\n' "$*" >&2; }
die() { log "$*"; exit 1; }

for tool in curl jq yq sha256sum base64; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
[[ -S "$DATA_DIR/run/.s.PGSQL.5432" ]] ||
  die "no managed PostgreSQL socket in $DATA_DIR/run: start the instance first (dev/local.sh serve) or pass --data-dir"
[[ -x "$BIN_DIR/cannery" ]] || die "no cannery binary in $BIN_DIR: pass --bin-dir"

# Session secrets, tokens, lease and upload headers live in a private
# directory in /tmp (RAM), never on disk, and never on a command line: curl
# reads each header set from a file.
SECRETS=$(umask 077 && mktemp -d /tmp/cannery-demo.XXXXXX)
TOKEN_IDS=()
cleanup() {
  local status=$?
  if [[ -f "$SECRETS/sessions-open" ]]; then
    for user in "${USERS[@]}"; do
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

# creds ACTOR: the header file of a user key, or of the current project's
# service account (agent, tester, evaluator).
creds() {
  case "$1" in
    agent|tester|evaluator) printf '%s' "$SECRETS/svc-$PROJECT-$1.h" ;;
    *) printf '%s' "$SECRETS/user-$1.h" ;;
  esac
}

# api ACTOR METHOD PATH [BODY] [HEADER_FILE]: prints the JSON answer; any
# status but 2xx stops the script with the API's error object.
api() {
  local actor=$1 method=$2 path=$3 body=${4-} extra=${5-} code
  local args=(-sS -o "$SECRETS/body" -w '%{http_code}' -X "$method" -H "@$(creds "$actor")")
  [[ -z "$extra" ]] || args+=(-H "@$extra")
  if [[ -n "$body" ]]; then
    args+=(-H 'Content-Type: application/json' --data-binary @-)
  fi
  code=$(printf '%s' "$body" | curl "${args[@]}" "$URL$path") || die "$method $path: request failed"
  if [[ "$code" != 2* ]]; then
    log "$method $path answered $code as $actor:"
    jq -c '.error // .' "$SECRETS/body" >&2 2>/dev/null || head -c 2000 "$SECRETS/body" >&2
    exit 1
  fi
  cat "$SECRETS/body"
}

# status_of ACTOR PATH: the HTTP status of a GET.
status_of() {
  curl -sS -o /dev/null -w '%{http_code}' -H "@$(creds "$1")" "$URL$2"
}

# headers_file NAME LINE...: writes a private header file and prints its path.
headers_file() {
  local file=$SECRETS/$1.h
  shift
  printf '%s\n' "$@" >"$file"
  printf '%s' "$file"
}

# upload ACTOR GRANT_PATH HEADER_FILE NAME_FIELD ROLE NAME MEDIA_TYPE FILE:
# one upload grant, then the bytes. Prints the manifest object.
upload() {
  local actor=$1 grant_path=$2 extra=$3 field=$4 role=$5 name=$6 media=$7 file=$8
  local size sha grant path code
  size=$(stat -c %s "$file")
  sha=$(sha256sum "$file" | cut -d' ' -f1)
  grant=$(api "$actor" POST "$grant_path" "$(jq -nc --arg role "$role" --arg field "$field" \
    --arg name "$name" --argjson size "$size" --arg sha "$sha" --arg media "$media" \
    '{role: $role, ($field): $name, size_bytes: $size, sha256: $sha, media_type: $media}')" "$extra")
  # The grant's URL is built from the server's public base URL; keep its path
  # and send it to the URL this script reaches.
  path=$(jq -r '.upload_url' <<<"$grant" | sed -E 's#^[a-z]+://[^/]+##')
  jq -r '.headers | to_entries[] | "\(.key): \(.value)"' <<<"$grant" >"$SECRETS/upload.h"
  code=$(curl -sS -o "$SECRETS/body" -w '%{http_code}' -X PUT -H "@$SECRETS/upload.h" \
    -H "Content-Type: $media" --data-binary "@$file" "$URL$path") || die "PUT $path failed"
  [[ "$code" == 2* ]] || { log "upload of $name answered $code:"; jq -c '.error // .' "$SECRETS/body" >&2; exit 1; }
  jq -c '{role, storage, size_bytes, sha256, media_type}' "$SECRETS/body"
}

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
ago() { date -u -d "@$(($(date +%s) - $1))" +%Y-%m-%dT%H:%M:%SZ; }
# A stable fake commit for a hypothesis key.
commit_of() { printf '%s' "$1" | sha256sum | cut -c1-7; }

# ---------------------------------------------------------------- database

open_sessions() {
  local sql=$SECRETS/sessions.sql secret csrf hash
  : >"$sql"
  for user in "${USERS[@]}"; do
    secret="cr_ses_$(head -c 32 /dev/urandom | base64 -w0 | tr '+/' '-_' | tr -d '=')"
    csrf=$(head -c 24 /dev/urandom | base64 -w0 | tr '+/' '-_' | tr -d '=')
    hash=$(printf '%s' "$secret" | sha256sum | cut -d' ' -f1)
    printf 'Cookie: cr_session=%s\nX-CSRF-Token: %s\n' "$secret" "$csrf" >"$SECRETS/user-$user.h"
    printf "INSERT INTO sessions (secret_hash, user_id, csrf_token, expires_at) SELECT '\\\\x%s'::bytea, id, '%s', now() + interval '2 hours' FROM users WHERE issuer = '%s' AND subject = '%s';\n" \
      "$hash" "$csrf" "$ISSUER" "$user" >>"$sql"
  done
  log "creating the demo users and their sessions, importing the bundles"
  # The SQL holds only digests of the session secrets.
  "$RUN" --volume "$DATA_DIR:/db" --volume "$DEMO:/demo:ro" --volume "$BIN_DIR:/opt/cannery:ro" \
    --memory 1g --timeout 15m "$DB_IMAGE" bash /demo/db-bootstrap.sh <"$sql"
  rm -f "$sql"
  : >"$SECRETS/sessions-open"
}

# ---------------------------------------------------------------- projects

# project_value JQ: a value of the current project's scenario.
pv() { jq -c "$@" "$SECRETS/project.json"; }
pvr() { jq -r "$@" "$SECRETS/project.json"; }

setup_project() {
  local slug=$PROJECT title description status user role email id name kind
  title=$(pvr '.title')
  description=$(pvr '.description // ""')
  status=$(status_of "$ADMIN" "/api/projects/$slug")
  # A project is created with its first track; the others follow below.
  local created=0
  if [[ "$status" == 404 ]]; then
    api "$ADMIN" POST /api/projects "$(jq -nc --arg s "$slug" --arg t "$title" --arg d "$description" \
      --argjson track "$(pv '.tracks[0] | {slug, title, description}')" \
      '{slug: $s, title: $t, description: $d, tracks: [$track]}')" >/dev/null
    created=1
  fi
  for user in $(pvr '.members | keys[]'); do
    role=$(pvr ".members.$user")
    id=$(user_id "${EMAIL[$user]}") || die "demo user $user is missing"
    api "$ADMIN" PUT "$BASE/members/$id" "{\"role\":\"$role\"}" >/dev/null
  done
  IFS=, read -ra emails <<<"$GRANT"
  for email in "${emails[@]}"; do
    email=${email// /}
    [[ -n "$email" ]] || continue
    if id=$(user_id "$email"); then
      api "$ADMIN" PUT "$BASE/members/$id" '{"role":"researcher"}' >/dev/null
    else
      log "no user $email yet (sign in once, then grant it in the web app); skipped"
    fi
  done
  # An imported project already has its science revision, from the import.
  if [[ "$status" == 404 ]]; then
    api "$ADMIN" POST "$BASE/config/science" "$(cat "$DEMO/projects/$slug/science.json")" >/dev/null
  fi
  api "$ADMIN" POST "$BASE/producers" "$(cat "$DEMO/projects/$slug/producer.json")" >/dev/null
  for kind in agent tester evaluator; do
    name=$(service_name "$kind")
    api "$ADMIN" POST "$BASE/service-accounts" "$(jq -nc --arg k "$kind" --arg n "$name" \
      --arg d "Demo $kind of $slug, played by dev/seed-demo.sh." '{kind: $k, name: $n, description: $d}')" >/dev/null
    local token
    token=$(api "$ADMIN" POST "$BASE/service-accounts/$name/tokens" \
      '{"name":"seed-demo","expires_in_days":1,"scopes":["read","write"]}')
    TOKEN_IDS+=("$(jq -r .id <<<"$token")")
    printf 'Authorization: Bearer %s\n' "$(jq -r .token <<<"$token")" >"$(creds "$kind")"
  done
  local track
  while read -r track; do
    api "$ADMIN" POST "$BASE/tracks" "$(jq -c '{slug, title, description}' <<<"$track")" >/dev/null
  done < <(pv ".tracks[$created:][]")
  # The brief, written by one of the project's researchers.
  api "$(pvr '.brief.by')" POST "$BASE/brief" \
    "$(pv '{document: .brief.document, expected_revision: 0}')" >/dev/null
}

user_id() {
  local found
  found=$(api "$ADMIN" GET "/api/users?email=$(jq -rn --arg e "$1" '$e|@uri')" | jq -r '.items[0].id // empty')
  [[ -n "$found" ]] && printf '%s' "$found"
}

service_name() {
  case "$1" in
    agent) printf 'demo-agent' ;;
    tester) jq -r '.tester.id' "$DEMO/projects/$PROJECT/science.json" ;;
    evaluator) jq -r '.evaluator.id' "$DEMO/projects/$PROJECT/science.json" ;;
  esac
}

# ---------------------------------------------------------------- hypotheses

declare -A NUMBER
# hv INDEX JQ: a value of hypothesis INDEX of the scenario.
hv() { jq -c ".hypotheses[$1]$2" "$SECRETS/project.json"; }
hvr() { jq -r ".hypotheses[$1]$2" "$SECRETS/project.json"; }

# document INDEX [REVISION_PATCH]: the hypothesis document with relations
# resolved from scenario keys to numbers.
document() {
  local numbers patch=${2:-}
  [[ -n "$patch" ]] || patch='{}'
  numbers=$(for key in "${!NUMBER[@]}"; do printf '%s\t%s\n' "$key" "${NUMBER[$key]}"; done |
    jq -Rn '[inputs | split("\t") | {(.[0]): (.[1] | tonumber)}] | add // {}')
  jq -c --argjson n "$numbers" --argjson patch "$patch" \
    '{schema_version: "0.2"} + .doc + $patch
     | if .relations then .relations |= map(if (.hypothesis | type) == "string" then .hypothesis = $n[.hypothesis] else . end) else . end' <<<"$(hv "$1" '')"
}

# planned INDEX: whether hypothesis INDEX goes through its track's plan: its
# scenario approves it in one step and its track does not opt out.
planned() {
  local track
  track=$(hvr "$1" .doc.track)
  [[ "$(pvr --arg t "$track" '[.tracks[] | select(.slug == $t)][0].plan')" != false ]] &&
    hv "$1" '.review // [] | length == 1 and .[0].action == "approve"' | grep -qx true
}

# targets INDEX: the scenario keys hypothesis INDEX's relations name.
targets() { hvr "$1" '.doc.relations // [] | .[] | .hypothesis | select(type == "string")'; }

# unit INDEX KEYS: the plan entry of hypothesis INDEX; KEYS (a JSON list) are the keys of
# the units planned with it, which its relations and context name by key.
unit() {
  local numbers
  numbers=$(for key in "${!NUMBER[@]}"; do printf '%s\t%s\n' "$key" "${NUMBER[$key]}"; done |
    jq -Rn '[inputs | split("\t") | {(.[0]): (.[1] | tonumber)}] | add // {}')
  jq -c --argjson n "$numbers" --argjson keys "$2" '
    def ref: . as $v | if type == "string" and ($keys | index([$v]) | not) then $n[$v] else . end;
    def link: if type == "string" then {unit: .} else {hypothesis: .} end;
    {key, title: .doc.title, question: .doc.question, intervention: .doc.intervention,
     acceptance: .doc.plan, relations: [.doc.relations // [] | .[] | {kind} + (.hypothesis | ref | link)],
     context: [.doc.relations // [] | .[] | select(.kind == "derived_from") | {kind: "unit", unit: (.hypothesis | ref), note: "the unit this one derives from"}],
     brief: (.brief // "# Why\n\n\(.doc.rationale)\n")}
    + (if .doc.control then {control: .doc.control} else {} end)
    + (if .doc.project_fields then {parameters: .doc.project_fields} else {} end)' <<<"$(hv "$1" '')"
}

# plan_track TRACK INDEX...: one plan revision of TRACK with these units,
# written by the track's planner and approved by its reviewer.
plan_track() {
  local track=$1 spec by reviewer reason keys i path item
  shift
  spec=$(pv --arg t "$track" '[.tracks[] | select(.slug == $t)][0]')
  by=$(jq -r --arg d "$(pvr .brief.by)" '.plan.by // $d' <<<"$spec")
  reviewer=$(jq -r --arg d "$(pvr .brief.by)" '.plan.review.by // $d' <<<"$spec")
  reason=$(jq -r '.plan.review.reason // "The units follow the approach, and each has its acceptance plan and the context it needs."' <<<"$spec")
  keys=$(for i in "$@"; do hvr "$i" .key; done | jq -Rsc 'split("\n") | map(select(. != ""))')
  path=$BASE/tracks/$track/plans
  local revision
  revision=$(api "$by" POST "$path" | jq -r .revision)
  api "$by" PUT "$path/draft/approach" "$(jq -c '{approach: (.plan.approach // .description)}' <<<"$spec")" >/dev/null
  for i in "$@"; do
    api "$by" POST "$path/draft/units" "$(unit "$i" "$keys")" >/dev/null
  done
  [[ "$(api "$by" GET "$path/draft/check" | jq -r .ready)" == true ]] || die "the $track plan is not ready"
  api "$by" POST "$path/draft/submission" >/dev/null
  api "$reviewer" POST "$path/$revision/review" "$(jq -nc --arg r "$reason" '{action: "approve", reason: $r}')" >/dev/null
  while read -r item; do
    NUMBER[$(jq -r .key <<<"$item")]=$(jq -r .number <<<"$item")
    log "#$(jq -r .number <<<"$item") $(jq -r .title <<<"$item") (plan $track r$revision)"
  done < <(api "$ADMIN" GET "$BASE/tracks/$track/units?limit=200" |
    jq -c --argjson keys "$keys" '.items[] | select(.key as $k | $keys | index([$k]))')
}

# plan_and_draft COUNT: every hypothesis of the scenario, through its track's
# plan or as a draft, in passes: a plan or draft is written once every
# hypothesis its relations name exists (or is planned with it).
plan_and_draft() {
  local count=$1 i track target ok progress
  local -A done=()
  while ((${#done[@]} < count)); do
    progress=0
    for track in $(pvr '.tracks[].slug'); do
      local -A batch=()
      for ((i = 0; i < count; i++)); do
        [[ -z "${done[$i]-}" && "$(hvr "$i" .doc.track)" == "$track" ]] && planned "$i" && batch[$(hvr "$i" .key)]=$i
      done
      # Drop the units naming a hypothesis that neither exists nor is planned here.
      local changed=1
      while ((changed)); do
        changed=0
        for key in "${!batch[@]}"; do
          for target in $(targets "${batch[$key]}"); do
            if [[ -z "${NUMBER[$target]-}" && -z "${batch[$target]-}" ]]; then
              unset "batch[$key]"; changed=1; break
            fi
          done
        done
      done
      if ((${#batch[@]})); then
        local indexes
        indexes=$(printf '%s\n' "${batch[@]}" | sort -n)
        # shellcheck disable=SC2086
        plan_track "$track" $indexes
        for i in $indexes; do done[$i]=1; done
        progress=1
      fi
      unset batch
    done
    for ((i = 0; i < count; i++)); do
      [[ -z "${done[$i]-}" ]] && ! planned "$i" || continue
      ok=1
      for target in $(targets "$i"); do [[ -n "${NUMBER[$target]-}" ]] || ok=0; done
      if ((ok)); then draft "$i"; done[$i]=1; progress=1; fi
    done
    ((progress)) || die "the scenario's relations cannot all be resolved"
  done
}

draft() {
  local i=$1 key by created number
  key=$(hvr "$i" .key)
  by=$(hvr "$i" .by)
  created=$(api "$by" POST "$BASE/hypotheses" "$(document "$i")")
  number=$(jq -r .number <<<"$created")
  NUMBER[$key]=$number
  log "#$number $(jq -r .title <<<"$created")"
  local step action
  while read -r step; do
    if jq -e 'has("revise")' <<<"$step" >/dev/null; then
      local current revision
      current=$(api "$by" GET "$BASE/hypotheses/$number")
      revision=$(jq -r .revision <<<"$current")
      api "$by" PUT "$BASE/hypotheses/$number" "$(jq -nc --argjson r "$revision" \
        --argjson d "$(document "$i" "$(jq -c .revise <<<"$step")")" '{expected_revision: $r, document: $d}')" >/dev/null
    else
      action=$(jq -r .action <<<"$step")
      local revision
      revision=$(api "$ADMIN" GET "$BASE/hypotheses/$number" | jq -r .revision)
      api "$(jq -r .by <<<"$step")" POST "$BASE/hypotheses/$number/draft-review" \
        "$(jq -c --argjson r "$revision" '{draft_revision: $r, action, reason}' <<<"$step")" >/dev/null
    fi
  done < <(hv "$i" '.review // [] | .[]')
}

# measurements ATTEMPT_JSON FIELD AUTHORITY: measurement objects for the
# results that have FIELD (claimed or verified), typed from the science
# revision's metric registry.
measurements() {
  jq -c --arg field "$2" --arg authority "$3" --slurpfile science "$DEMO/projects/$PROJECT/science.json" '
    ($science[0].metrics | map({(.key): .}) | add) as $registry
    | [.results[]
      | select(has($field) or ($authority == "tester_verified" and has("missing_reason")))
      | {
        metric, split,
        dimensions: (.dims // {}),
        authority: $authority,
        unit: $registry[.metric].unit,
        direction: $registry[.metric].direction
      }
      + (if has($field) then {value: .[$field]} else {missing_reason} end)
      + (if .n then {sample_count: .n} else {} end)
      + (if .control and $authority == "tester_verified" then {control_value: .control} else {} end)
      + (if .ci and $authority == "tester_verified" then {uncertainty: {method: "bootstrap, 95%, 1000 resamples", lower: .ci[0], upper: .ci[1]}} else {} end)
    ]' <<<"$1"
}

# run_attempt INDEX ATTEMPT_INDEX: claim, then go as far as the scenario's
# `stop` says.
run_attempt() {
  local i=$1 a=$2 key number spec by stop claim lease_file attempt_id sequence path
  key=$(hvr "$i" .key)
  number=${NUMBER[$key]}
  spec=$(hv "$i" ".attempts[$a]")
  by=$(jq -r .by <<<"$spec")
  stop=$(jq -r .stop <<<"$spec")
  claim=$(api "$by" POST "$BASE/claims" "{\"hypothesis\":$number}")
  attempt_id=$(jq -r .attempt.id <<<"$claim")
  sequence=$(jq -r .attempt.sequence <<<"$claim")
  path=$BASE/hypotheses/$number/attempts/$sequence
  lease_file=$(headers_file "lease" \
    "X-Lease-Token: $(jq -r .lease_token <<<"$claim")" \
    "X-Lease-Generation: $(jq -r .lease_generation <<<"$claim")")
  api "$by" POST "$path/heartbeat" '{}' "$lease_file" >/dev/null
  log "  #$number.$sequence claimed by $by"
  case "$stop" in
    running) return ;;
    released)
      api "$by" POST "$path/release" "$(jq -c '{reason: .release}' <<<"$spec")" "$lease_file" >/dev/null
      if jq -e .failure_decision <<<"$spec" >/dev/null; then
        decide "$number" failure "$(jq -c .failure_decision <<<"$spec")"
      fi
      return ;;
  esac

  # The candidate and a training log, then the manifest and the sheet.
  local work=$SECRETS/work objects manifest seconds sheet
  mkdir -p "$work"
  jq '.candidate' <<<"$spec" >"$work/candidate.json"
  seconds=$(jq -r '.seconds // 3600' <<<"$spec")
  {
    printf '%s demo run of hypothesis %s (%s)\n' "$(ago "$seconds")" "$number" "$key"
    jq -r '.results[] | select(has("claimed")) | "  \(.metric) \(.split) \(.dims // {} | to_entries | map("\(.key)=\(.value)") | join(",")) claimed \(.claimed)"' <<<"$spec"
    printf '%s done\n' "$(now)"
  } >"$work/train.log"
  objects=$(upload "$by" "$path/uploads" "$lease_file" name candidate candidate.json application/json "$work/candidate.json")
  objects+=$'\n'$(upload "$by" "$path/uploads" "$lease_file" name training_log train.log text/plain "$work/train.log")
  manifest=$(api "$by" POST "$path/manifest" "$(jq -sc --arg id "$attempt_id" \
    '{schema_version: "0.2", attempt_id: $id, objects: .}' <<<"$objects")" "$lease_file")
  sheet=$(jq -c --arg id "$attempt_id" --argjson manifest "$manifest" \
    --argjson measurements "$(measurements "$spec" claimed agent_claim)" \
    --arg started "$(ago "$seconds")" --arg finished "$(now)" --arg commit "$(commit_of "$key")" \
    --arg science "$(jq -r .attempt.science_revision <<<"$claim")" --arg by "$by" '
    {
      schema_version: "0.2", attempt_id: $id, stage: "agent", status: "completed",
      producer: {kind: "agent", id: (if $by == "agent" then "demo-agent" else "demo-notebook-\($by)" end)},
      started_at: $started, finished_at: $finished,
      provenance: {source_revision: $commit, science_revision: $science},
      measurements: $measurements,
      observations: .sheet.observations,
      artifact_roles: ["candidate", "training_log"],
      manifest: $manifest,
      report: {
        what_was_tried: .sheet.tried, configuration: .sheet.configuration,
        observations: .sheet.observations, findings: .sheet.findings,
        limitations: .sheet.limitations, next_question: .sheet.next_question,
        elapsed_seconds: (.seconds // 3600), body_markdown: .sheet.body
      },
      extensions: {}
    }' <<<"$spec")
  api "$by" POST "$path/submission" "$sheet" \
    "$(headers_file submit "$(cat "$lease_file")" "Idempotency-Key: seed-demo-$attempt_id")" >/dev/null
  [[ "$stop" != submitted ]] || return 0

  run_test_job "$attempt_id" "$key" "$spec"
  [[ "$stop" != tested ]] || return 0
  run_evaluation_job "$attempt_id" "$key" "$spec"
  if jq -e .decision <<<"$spec" >/dev/null; then
    decide "$number" result "$(jq -c .decision <<<"$spec")"
  fi
}

run_test_job() {
  local attempt_id=$1 key=$2 spec=$3 claimed job job_id lease_file evidence objects scorer
  claimed=$(api tester POST "$BASE/jobs/claims" '{}')
  job=$(jq -c .job <<<"$claimed")
  [[ "$(jq -r .attempt_id <<<"$job")" == "$attempt_id" ]] ||
    die "the tester claimed a test job of another attempt; is something else using this project?"
  job_id=$(jq -r .job_id <<<"$job")
  lease_file=$(headers_file job-lease \
    "X-Lease-Token: $(jq -r .lease.token <<<"$job")" \
    "X-Lease-Generation: $(jq -r .lease.generation <<<"$job")")
  scorer=$(jq -r '.scorer.metadata.name' "$DEMO/projects/$PROJECT/science.json")
  evidence=$(jq -c --argjson job "$job" --argjson measurements "$(measurements "$spec" verified tester_verified)" \
    --arg started "$(ago 240)" --arg finished "$(now)" --arg commit "$(commit_of "$key")" \
    --arg dataset "$(pvr .dataset_revision)" --arg tester "$(service_name tester)" '
    {
      schema_version: "0.2", attempt_id: $job.attempt_id, stage: "tester", status: "completed",
      producer: {kind: "service", id: $tester},
      started_at: $started, finished_at: $finished,
      provenance: ({
        source_revision: $commit, tester_revision: $job.tester.revision,
        dataset_revision: $dataset, science_revision: $job.science_revision
      } + (if $job.control then {control_revision: $job.control.revision} else {} end)),
      observations: (.tester_notes // "Re-ran the frozen submission with the registered producer and scorer."),
      measurements: $measurements,
      discrepancies: [. as $spec | .discrepancies // [] | .[] as $d
        | ([$spec.results[] | select(.metric == $d.metric and .split == $d.split and ((.dims // {}) == ($d.dims // {})))][0]) as $r
        | {metric: $d.metric, split: $d.split, dimensions: ($d.dims // {}),
           claimed_value: $r.claimed, verified_value: $r.verified, description: $d.description}],
      artifact_roles: ["evidence", "step_log"]
    }' <<<"$spec")
  local work=$SECRETS/work
  printf '%s\n' "$evidence" >"$work/evidence.json"
  printf '%s %s: staged inputs, ran the producer and the scorer (demo)\n' "$(now)" "$scorer" >"$work/scorer.log"
  objects=$(upload tester "$BASE/jobs/$job_id/uploads" "$lease_file" path evidence \
    "$scorer/evidence/evidence.json" application/json "$work/evidence.json")
  objects+=$'\n'$(upload tester "$BASE/jobs/$job_id/uploads" "$lease_file" path step_log \
    "$scorer/step_log/$scorer.log" text/plain "$work/scorer.log")
  api tester POST "$BASE/jobs/$job_id/completion" "$(jq -nc --arg job "$job_id" --argjson evidence "$evidence" \
    --argjson objects "$(jq -sc . <<<"$objects")" \
    '{schema_version: "0.2", job_id: $job, evidence: $evidence,
      manifest: {schema_version: "0.2", attempt_id: $evidence.attempt_id, objects: $objects}}')" "$lease_file" >/dev/null
}

run_evaluation_job() {
  local attempt_id=$1 key=$2 spec=$3 revision claimed job job_id lease_file record
  revision=$(jq -r '.evaluator.revision' "$DEMO/projects/$PROJECT/science.json")
  claimed=$(api evaluator POST "$BASE/jobs/claims" "{\"stage\":\"evaluator\",\"revision\":\"$revision\"}")
  job=$(jq -c .job <<<"$claimed")
  [[ "$(jq -r .attempt_id <<<"$job")" == "$attempt_id" ]] ||
    die "the evaluator claimed an evaluation job of another attempt; is something else using this project?"
  job_id=$(jq -r .job_id <<<"$job")
  lease_file=$(headers_file job-lease \
    "X-Lease-Token: $(jq -r .lease.token <<<"$job")" \
    "X-Lease-Generation: $(jq -r .lease.generation <<<"$job")")
  record=$(jq -c --argjson job "$job" --arg started "$(ago 5)" --arg finished "$(now)" \
    --arg evaluator "$(service_name evaluator)" --arg dataset "$(pvr .dataset_revision)" --arg commit "$(commit_of "$key")" '
    . as $spec
    | {
      schema_version: "0.2", attempt_id: $job.attempt_id, stage: "evaluator", status: "completed",
      producer: {kind: "service", id: $evaluator},
      started_at: $started, finished_at: $finished,
      provenance: ({source_revision: $commit, dataset_revision: $dataset, science_revision: $job.science_revision}
        + (if $job.control then {control_revision: $job.control.revision} else {} end)),
      assessment: ({
        policy_revision: $job.evaluator.revision,
        gates: .verdict.gates,
        evidence: $job.inputs.evidence,
        verdict: .verdict.result,
        reason: .verdict.reason
      } + (if .verdict.compare then {comparisons: [.verdict.compare[] as $c
        | {metric: $c.metric, split: $c.split, dimensions: ($c.dims // {}),
           value: ([$spec.results[] | select(.metric == $c.metric and .split == $c.split
             and ((.dims // {}) == ($c.dims // {})))][0].verified),
           source: "tester", reference: $c.reference}]} else {} end))
    }' <<<"$spec")
  api evaluator POST "$BASE/jobs/$job_id/completion" \
    "$(jq -nc --arg job "$job_id" --argjson record "$record" '{schema_version: "0.2", job_id: $job, evidence: $record}')" \
    "$lease_file" >/dev/null
}

# decide NUMBER KIND DECISION_JSON: the pending review case of that kind.
decide() {
  local number=$1 kind=$2 decision=$3 case
  case=$(api "$ADMIN" GET "$BASE/review-cases?kind=$kind&state=pending&limit=200" |
    jq -c --argjson n "$number" '[.items[] | select(.hypothesis == $n)][0] // empty')
  [[ -n "$case" ]] || die "no pending $kind review case for #$number"
  api "$(jq -r .by <<<"$decision")" POST "$BASE/review-cases/$(jq -r .id <<<"$case")/decisions" \
    "$(jq -c --argjson case "$case" '{review_case_id: $case.id, evidence_revision: $case.subject_revision, action, reason}' <<<"$decision")" >/dev/null
}

comments() {
  local i=$1 key number comment on target created
  key=$(hvr "$i" .key)
  number=${NUMBER[$key]}
  while read -r comment; do
    on=$(jq -r .on <<<"$comment")
    if [[ "$on" == hypothesis ]]; then
      target=$BASE/hypotheses/$number/comments
    else
      target=$BASE/hypotheses/$number/attempts/$on/comments
    fi
    created=$(api "$(jq -r .by <<<"$comment")" POST "$target" "$(jq -c '{body_markdown: .body}' <<<"$comment")")
    if jq -e .edit <<<"$comment" >/dev/null; then
      api "$(jq -r .by <<<"$comment")" PUT "$BASE/comments/$(jq -r .id <<<"$created")" \
        "$(jq -c --argjson r "$(jq .revision <<<"$created")" '{expected_revision: $r, body_markdown: .edit}' <<<"$comment")" >/dev/null
    fi
  done < <(hv "$i" '.comments // [] | .[]')
}

final_track_states() {
  local track slug current
  while read -r track; do
    slug=$(jq -r .slug <<<"$track")
    current=$(api "$ADMIN" GET "$BASE/tracks/$slug")
    api "$(jq -r .final.by <<<"$track")" POST "$BASE/tracks/$slug/transitions" \
      "$(jq -c --argjson r "$(jq .revision <<<"$current")" '{to_state: .final.state, expected_revision: $r, reason: .final.reason}' <<<"$track")" >/dev/null
    log "track $slug $(jq -r .final.state <<<"$track")"
  done < <(pv '.tracks[] | select(.final)')
}

load_project() {
  PROJECT=$1
  BASE=/api/projects/$PROJECT
  NUMBER=()
  yaml_json "$DEMO/projects/$PROJECT/project.yaml" >"$SECRETS/project.json"
  log "project $PROJECT"
  setup_project
  local count i a attempts order group
  count=$(pv '.hypotheses | length')
  # Plans and drafts: the hypotheses a scenario approves in one step are units
  # of their track's plan, the others drafts; each is written once what its
  # relations name exists.
  plan_and_draft "$count"
  # Attempts. A claim of a test or evaluation job cannot name its attempt, so
  # hypotheses that leave a job pending go last: completed work first, then
  # attempts waiting for evaluation, then for testing, then the running one.
  order=$(pv '[.hypotheses | to_entries[] | select(.value.attempts)
    | {i: .key, g: ({"tested": 1, "submitted": 2, "running": 3}[.value.attempts[-1].stop] // 0)}]
    | sort_by(.g, .i) | .[].i')
  for i in $order; do
    attempts=$(hv "$i" '.attempts | length')
    log "#${NUMBER[$(hvr "$i" .key)]} attempts"
    for ((a = 0; a < attempts; a++)); do run_attempt "$i" "$a"; done
  done
  for ((i = 0; i < count; i++)); do comments "$i"; done
  final_track_states
}

# ---------------------------------------------------------------- main

curl -fsS -o /dev/null "$URL/api/health" || die "$URL/api/health does not answer: is the instance running?"
open_sessions
api "$ADMIN" GET /api/me >/dev/null
for project in $PROJECTS; do
  # An imported project exists already; its live tracks do not.
  if [[ "$(status_of "$ADMIN" "/api/projects/$project/tracks/$(jq -r '.tracks[0].slug' <(yaml_json "$DEMO/projects/$project/project.yaml"))")" != 404 ]]; then
    die "$project already has demo data; delete the instance's data directory to start over"
  fi
done
for project in $PROJECTS; do
  load_project "$project"
done

for id in "${TOKEN_IDS[@]}"; do
  [[ "$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE -H "@$(creds "$ADMIN")" "$URL/api/tokens/$id")" == 2* ]] ||
    log "could not revoke service token $id; it expires within a day"
done
log "done: open $URL"
