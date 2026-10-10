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
# Tracks start in planning. Every unit comes from a plan: each one of
# the scenario is a unit of its track's plan. The track's planner (plan.by,
# default brief.by) starts a plan revision, sets the approach (plan.approach,
# default the description) and adds the units, with the rationale as the unit
# brief and a context item per derived_from relation, then checks and submits
# it, and the reviewer (plan.review.by, default brief.by) approves it, which
# creates the units, queued, and makes the track active. A unit naming a
# unit that does not exist yet waits for a later revision of the plan.
# A unit with later: goes into one more revision of the plan once the
# attempts are done, which keeps every unit in flight or done: the reviewer
# later.by declines it with later.reason (later: {review: decline}), or it is
# left for review (later: {review: pending}).
#
# The REST part drives each unit of dev/demo-data/projects/*/project.yaml
# through the real lifecycle: plans and their reviews, claims, uploads,
# claimed run documents, verify jobs completed with a verification report,
# write-ups (an agent's, a researcher's, a skipped one), decision documents,
# failure reviews, comments and track transitions. There is
# no runner: the script plays the agent and the verifier itself. A project
# whose science revision registers a runner verifier gets a verifier service
# account of that name; in one whose agents verify, a researcher verifies the
# agent's attempts and the agent service account the researchers' ones. The
# attempt left running keeps its lease for leases.ttl_seconds (15 minutes by
# default), then the sweep fails it with lease_expired and opens a failure
# review, as for a crashed agent; a verify job left claimed (stop: verifying)
# goes back to waiting the same way, as for a crashed verifier. A verified or
# stopped unit whose last attempt has no writeup: is left waiting for its
# write-up, and one with a writeup: but no decision: waits for its decision.
# A project whose science revision registers a decider step (decide.performer
# step) gets a decider service account of that name, and the script plays its
# runner: a decision by: decider is the decider's, recorded through its decide
# job; a unit written up without one leaves that job waiting.
#
# Once the attempts and the later plans are done, the scenario's concerns:
# about the plans are raised by their authors (from: names the unit and
# attempt), and a dismissed: one is dismissed by a researcher with a reason;
# an open one holds up its track.
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
# service account (agent, verifier, decider).
creds() {
  case "$1" in
    agent|verifier|decider) printf '%s' "$SECRETS/svc-$PROJECT-$1.h" ;;
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
# A stable fake commit for a unit key.
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
  local kinds=agent
  [[ "$(verify_performer)" != runner ]] || kinds+=" verifier"
  [[ "$(decide_performer)" != step ]] || kinds+=" decider"
  for kind in $kinds; do
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
    verifier) jq -r '.verify.verifier.id' "$DEMO/projects/$PROJECT/science.json" ;;
    decider) jq -r '.decide.decider.id' "$DEMO/projects/$PROJECT/science.json" ;;
  esac
}

# verify_performer: who verifies the current project's attempts, runner or agent.
verify_performer() { jq -r '.verify.performer' "$DEMO/projects/$PROJECT/science.json"; }
# decide_performer: who decides them, researcher (the default) or step.
decide_performer() { jq -r '.decide.performer // "researcher"' "$DEMO/projects/$PROJECT/science.json"; }

# ---------------------------------------------------------------- units

declare -A NUMBER
# hv INDEX JQ: a value of unit INDEX of the scenario.
hv() { jq -c ".units[$1]$2" "$SECRETS/project.json"; }
hvr() { jq -r ".units[$1]$2" "$SECRETS/project.json"; }

# planned INDEX: whether unit INDEX is a unit of its track's approved
# plan, rather than of a later revision (later).
planned() { hv "$1" ' | has("later") | not' | grep -qx true; }

# targets INDEX: the scenario keys unit INDEX's relations name.
targets() { hvr "$1" '.doc.relations // [] | .[] | .unit | select(type == "string")'; }

# unit INDEX KEYS: the plan entry of unit INDEX; KEYS (a JSON list) are the keys of
# the units planned with it, which its relations and context name by key.
unit() {
  local numbers
  numbers=$(for key in "${!NUMBER[@]}"; do printf '%s\t%s\n' "$key" "${NUMBER[$key]}"; done |
    jq -Rn '[inputs | split("\t") | {(.[0]): (.[1] | tonumber)}] | add // {}')
  jq -c --argjson n "$numbers" --argjson keys "$2" '
    def ref: . as $v | if type == "string" and ($keys | index([$v]) | not) then $n[$v] else . end;
    def link: if type == "string" then {unit: .} else {unit: .} end;
    {key, title: .doc.title, question: .doc.question, intervention: .doc.intervention,
     acceptance: .doc.plan, relations: [.doc.relations // [] | .[] | {kind} + (.unit | ref | link)],
     context: [.doc.relations // [] | .[] | select(.kind == "derived_from") | {kind: "unit", unit: (.unit | ref), note: "the unit this one derives from"}],
     brief: (.brief // "# Why\n\n\(.doc.rationale)\n")}
    + (if .doc.control then {control: .doc.control} else {} end)
    + (if .doc.project_fields then {parameters: .doc.project_fields} else {} end)' <<<"$(hv "$1" '')"
}

# write_plan TRACK INDEX...: a new plan revision of TRACK with these units,
# written by the track's planner, every unit in flight or done kept, checked
# and submitted. Prints the revision number.
write_plan() {
  local track=$1 spec by keys i path revision number
  shift
  spec=$(pv --arg t "$track" '[.tracks[] | select(.slug == $t)][0]')
  by=$(jq -r --arg d "$(pvr .brief.by)" '.plan.by // $d' <<<"$spec")
  keys=$(for i in "$@"; do hvr "$i" .key; done | jq -Rsc 'split("\n") | map(select(. != ""))')
  path=$BASE/tracks/$track/plans
  revision=$(api "$by" POST "$path" | jq -r .revision)
  api "$by" PUT "$path/draft/approach" "$(jq -c '{approach: (.plan.approach // .description)}' <<<"$spec")" >/dev/null
  for i in "$@"; do
    api "$by" POST "$path/draft/units" "$(unit "$i" "$keys")" >/dev/null
  done
  for number in $(api "$by" GET "$path/draft" | jq -r '.needs_alignment[].number'); do
    api "$by" PUT "$path/draft/alignments/$number" '{"decision": "keep", "reason": "Still part of the plan."}' >/dev/null
  done
  [[ "$(api "$by" GET "$path/draft/check" | jq -r .ready)" == true ]] || die "the $track plan is not ready"
  api "$by" POST "$path/draft/submission" >/dev/null
  echo "$revision"
}

# plan_track TRACK INDEX...: one plan revision of TRACK with these units,
# approved by the track's reviewer, which creates their units.
plan_track() {
  local track=$1 spec reviewer reason keys revision item
  shift
  spec=$(pv --arg t "$track" '[.tracks[] | select(.slug == $t)][0]')
  reviewer=$(jq -r --arg d "$(pvr .brief.by)" '.plan.review.by // $d' <<<"$spec")
  reason=$(jq -r '.plan.review.reason // "The units follow the approach, and each has its acceptance plan and the context it needs."' <<<"$spec")
  keys=$(for i in "$@"; do hvr "$i" .key; done | jq -Rsc 'split("\n") | map(select(. != ""))')
  revision=$(write_plan "$track" "$@")
  api "$reviewer" POST "$BASE/tracks/$track/plans/$revision/review" "$(jq -nc --arg r "$reason" '{action: "approve", reason: $r}')" >/dev/null
  while read -r item; do
    NUMBER[$(jq -r .key <<<"$item")]=$(jq -r .number <<<"$item")
    log "#$(jq -r .number <<<"$item") $(jq -r .title <<<"$item") (plan $track r$revision)"
  done < <(api "$ADMIN" GET "$BASE/tracks/$track/units?limit=200" |
    jq -c --argjson keys "$keys" '.items[] | select(.key as $k | $keys | index([$k]))')
}

# plan_all COUNT: every planned unit of the scenario, through its
# track's plan, in passes: a revision is written once every unit its
# units' relations name exists (or is planned with it).
plan_all() {
  local count=$1 i track target
  local -A done=()
  for ((i = 0; i < count; i++)); do planned "$i" || done[$i]=1; done
  while ((${#done[@]} < count)); do
    local progress=0
    for track in $(pvr '.tracks[].slug'); do
      local -A batch=()
      for ((i = 0; i < count; i++)); do
        [[ -z "${done[$i]-}" && "$(hvr "$i" .doc.track)" == "$track" ]] && batch[$(hvr "$i" .key)]=$i
      done
      # Drop the units naming a unit that neither exists nor is planned here.
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
    ((progress)) || die "the scenario's relations cannot all be resolved"
  done
}

# later_plans COUNT: the units of a later revision of their track's plan
# (later.review): a revision the reviewer declined (decline, by later.by with
# later.reason), and one submitted and left for review (pending).
later_plans() {
  local count=$1 track review i first revision
  for track in $(pvr '.tracks[].slug'); do
    for review in decline pending; do
      local indexes=()
      for ((i = 0; i < count; i++)); do
        [[ "$(hvr "$i" .doc.track)" == "$track" && "$(hvr "$i" '.later.review // ""')" == "$review" ]] && indexes+=("$i")
      done
      ((${#indexes[@]})) || continue
      revision=$(write_plan "$track" "${indexes[@]}")
      first=${indexes[0]}
      if [[ "$review" == decline ]]; then
        api "$(hvr "$first" .later.by)" POST "$BASE/tracks/$track/plans/$revision/review" \
          "$(hv "$first" '.later | {action: "decline", reason}')" >/dev/null
        log "plan $track r$revision declined"
      else
        log "plan $track r$revision awaiting review"
      fi
    done
  done
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
  number=${NUMBER[$key]-}
  [[ -n "$number" ]] || return 0
  spec=$(hv "$i" ".attempts[$a]")
  by=$(jq -r .by <<<"$spec")
  stop=$(jq -r .stop <<<"$spec")
  claim=$(api "$by" POST "$BASE/claims" "{\"unit\":$number}")
  attempt_id=$(jq -r .attempt.id <<<"$claim")
  sequence=$(jq -r .attempt.sequence <<<"$claim")
  path=$BASE/units/$number/attempts/$sequence
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
        # A stopped unit is written up, then decided failed.
        [[ "$(jq -r .failure_decision.action <<<"$spec")" != stop ]] || finish "$number" "$spec"
      fi
      return ;;
  esac

  # The candidate and a training log, then the manifest and the run document.
  local work=$SECRETS/work objects manifest seconds front notes document
  mkdir -p "$work"
  jq '.candidate' <<<"$spec" >"$work/candidate.json"
  seconds=$(jq -r '.seconds // 3600' <<<"$spec")
  {
    printf '%s demo run of unit %s (%s)\n' "$(ago "$seconds")" "$number" "$key"
    jq -r '.results[] | select(has("claimed")) | "  \(.metric) \(.split) \(.dims // {} | to_entries | map("\(.key)=\(.value)") | join(",")) claimed \(.claimed)"' <<<"$spec"
    printf '%s done\n' "$(now)"
  } >"$work/train.log"
  objects=$(upload "$by" "$path/uploads" "$lease_file" name candidate candidate.json application/json "$work/candidate.json")
  objects+=$'\n'$(upload "$by" "$path/uploads" "$lease_file" name training_log train.log text/plain "$work/train.log")
  manifest=$(api "$by" POST "$path/manifest" "$(jq -sc --arg id "$attempt_id" \
    '{schema_version: "0.2", attempt_id: $id, objects: .}' <<<"$objects")" "$lease_file")
  # The front matter, one JSON value per key (JSON is YAML), and the notes.
  front=$(jq -r --argjson manifest "$manifest" \
    --argjson claims "$(measurements "$spec" claimed agent_claim)" \
    --arg commit "$(commit_of "$key")" --arg science "$(jq -r .attempt.science_revision <<<"$claim")" '
    {
      claims: $claims,
      provenance: {source_revision: $commit, science_revision: $science},
      artifact_roles: ["candidate", "training_log"],
      manifest: $manifest,
      extensions: {}
    } | to_entries[] | "\(.key): \(.value | tojson)"' <<<"$spec")
  notes=$(jq -r '.sheet | .body // ([
      "## What was tried", .tried, "## Configuration", .configuration,
      "## Observations", .observations, "## Findings", .findings,
      "## Limitations", .limitations, "## Next question", .next_question
    ] | join("\n\n"))' <<<"$spec")
  document=$(printf -- '---\n%s\n---\n%s\n' "$front" "$notes")
  api "$by" POST "$path/submission" "$(jq -nc --arg document "$document" '{document: $document}')" \
    "$(headers_file submit "$(cat "$lease_file")" "Idempotency-Key: seed-demo-$attempt_id")" >/dev/null
  [[ "$stop" != submitted ]] || return 0

  run_verify_job "$attempt_id" "$key" "$spec"
  [[ "$stop" != verifying ]] || return 0
  finish "$number" "$spec"
}

# run_verify_job ATTEMPT_ID KEY SPEC: claim the attempt's verify job, upload
# the step outputs the science revision requires and complete it with a
# verification report. A runner verifier claims under its policy revision; in
# a project whose agents verify, the attempt's verifier claims (default the
# brief's author for the agent's attempts, the agent for a researcher's).
# With stop: verifying the job stays claimed.
run_verify_job() {
  local attempt_id=$1 key=$2 spec=$3 science=$DEMO/projects/$PROJECT/science.json
  local verifier request claimed job job_id lease_file scorer role objects='' policy front body document
  if [[ "$(verify_performer)" == runner ]]; then
    verifier=verifier
    request=$(jq -c '{phase: "verify", revision: .verify.verifier.revision}' "$science")
  elif [[ "$(jq -r .by <<<"$spec")" == agent ]]; then
    verifier=$(jq -r --arg d "$(pvr .brief.by)" '.verifier // $d' <<<"$spec")
    request='{"phase":"verify"}'
  else
    verifier=$(jq -r '.verifier // "agent"' <<<"$spec")
    request='{"phase":"verify"}'
  fi
  claimed=$(api "$verifier" POST "$BASE/jobs/claims" "$request")
  job=$(jq -c .job <<<"$claimed")
  [[ "$(jq -r .attempt_id <<<"$job")" == "$attempt_id" ]] ||
    die "$verifier claimed a verify job of another attempt; is something else using this project?"
  job_id=$(jq -r .job_id <<<"$job")
  lease_file=$(headers_file job-lease \
    "X-Lease-Token: $(jq -r .lease.token <<<"$job")" \
    "X-Lease-Generation: $(jq -r .lease.generation <<<"$job")")
  log "  verify job claimed by $verifier"
  [[ "$(jq -r .stop <<<"$spec")" != verifying ]] || return 0

  local work=$SECRETS/work
  scorer=$(jq -r '.scorer.metadata.name' "$science")
  for role in $(jq -r '.required_artifact_roles.verify[]' "$science"); do
    case "$role" in
      evidence)
        measurements "$spec" verified tester_verified | jq '{measurements: .}' >"$work/evidence.json"
        objects+=$(upload "$verifier" "$BASE/jobs/$job_id/uploads" "$lease_file" path evidence \
          "$scorer/evidence/evidence.json" application/json "$work/evidence.json")$'\n' ;;
      step_log)
        printf '%s %s: staged inputs, ran the producer and the scorer (demo)\n' "$(now)" "$scorer" >"$work/scorer.log"
        objects+=$(upload "$verifier" "$BASE/jobs/$job_id/uploads" "$lease_file" path step_log \
          "$scorer/step_log/$scorer.log" text/plain "$work/scorer.log")$'\n' ;;
      *) die "no demo output for the verify role $role" ;;
    esac
  done
  # A runner applies its registered policy revision; a person or an agent names its own.
  policy=$(jq -r '.verify.verifier.revision // "demo-review-checklist-1"' "$science")
  # The front matter, one JSON value per key (JSON is YAML), and the notes.
  front=$(jq -r --argjson job "$job" --arg policy "$policy" \
    --argjson measurements "$(measurements "$spec" verified tester_verified)" \
    --argjson roles "$(jq -c '.required_artifact_roles.verify' "$science")" \
    --arg commit "$(commit_of "$key")" --arg dataset "$(pvr .dataset_revision)" '
    . as $spec
    | {
      verdict: .verdict.result,
      reason: .verdict.reason,
      policy_revision: $policy,
      gates: .verdict.gates,
      measurements: $measurements,
      discrepancies: [.discrepancies // [] | .[] as $d
        | ([$spec.results[] | select(.metric == $d.metric and .split == $d.split and ((.dims // {}) == ($d.dims // {})))][0]) as $r
        | {metric: $d.metric, split: $d.split, dimensions: ($d.dims // {}),
           claimed_value: $r.claimed, verified_value: $r.verified, description: $d.description}],
      comparisons: [.verdict.compare // [] | .[] as $c
        | {metric: $c.metric, split: $c.split, dimensions: ($c.dims // {}),
           value: ([$spec.results[] | select(.metric == $c.metric and .split == $c.split
             and ((.dims // {}) == ($c.dims // {})))][0].verified),
           source: "tester", reference: $c.reference}],
      provenance: ({source_revision: $commit, dataset_revision: $dataset, science_revision: $job.science_revision}
        + (if $job.control then {control_revision: $job.control.revision} else {} end)),
      artifact_roles: $roles
    } | to_entries[] | "\(.key): \(.value | tojson)"' <<<"$spec")
  body=$(jq -r '.notes // "Re-ran the frozen run with the registered producer and scorer."' <<<"$spec")
  document=$(printf -- '---\n%s\n---\n%s\n' "$front" "$body")
  api "$verifier" POST "$BASE/jobs/$job_id/completion" "$(jq -nc --arg job "$job_id" --arg id "$attempt_id" \
    --arg document "$document" --argjson objects "$(jq -sc . <<<"$objects")" \
    '{schema_version: "0.2", job_id: $job, document: $document,
      manifest: {schema_version: "0.2", attempt_id: $id, objects: $objects}}')" "$lease_file" >/dev/null
}

# decide NUMBER KIND DECISION_JSON: the pending review case of that kind. A
# failure case takes the action and the reason; a decision case takes a
# decision document citing the verification report and the write-up it
# decides on (each null when there is none), with the reason as its body.
decide() {
  local number=$1 kind=$2 decision=$3 case writeup body
  case=$(api "$ADMIN" GET "$BASE/review-cases?kind=$kind&state=pending&limit=200" |
    jq -c --argjson n "$number" '[.items[] | select(.unit == $n)][0] // empty')
  [[ -n "$case" ]] || die "no pending $kind review case for #$number"
  if [[ "$kind" == decision && "$(jq -r .by <<<"$decision")" == decider ]]; then
    decide_automatically "$number" "$decision"
    return
  fi
  if [[ "$kind" == failure ]]; then
    body=$(jq -c --argjson case "$case" '{review_case_id: $case.id, evidence_revision: $case.subject_revision, action, reason}' <<<"$decision")
  else
    writeup=$(api "$ADMIN" GET "$BASE/units/$number/writeup")
    body=$(jq -c --argjson case "$case" --argjson w "$writeup" '{review_case_id: $case.id, document: (
      "---\noutcome: \(.action)\nverification: \($w.inputs.verification // null | tojson)\nwriteup: \(
        if $w.status == "written" then {ref: $w.writeup.id, sha256: $w.writeup.sha256} else null end | tojson)\n---\n\n\(.reason)\n")}' <<<"$decision")
  fi
  api "$(jq -r .by <<<"$decision")" POST "$BASE/review-cases/$(jq -r .id <<<"$case")/decisions" "$body" >/dev/null
}

# decide_automatically NUMBER DECISION_JSON: the decider step's decision, as
# the runner's decide kind would record it: the decider service account
# claims the unit's decide job under its step revision and completes it
# with the decision document, citing what the job names.
decide_automatically() {
  local number=$1 decision=$2 job lease_file document
  job=$(api decider POST "$BASE/jobs/claims" \
    "$(jq -c '{phase: "decide", revision: .decide.decider.revision}' "$DEMO/projects/$PROJECT/science.json")" | jq -c .job)
  [[ "$(jq -r .unit <<<"$job")" == "$number" ]] ||
    die "the decider claimed the decide job of another unit; is something else using this project?"
  document=$(jq -r --argjson inputs "$(jq -c .inputs <<<"$job")" \
    '"---\noutcome: \(.action)\nverification: \($inputs.verification // null | tojson)\nwriteup: \($inputs.writeup // null | tojson)\n---\n\n\(.reason)\n"' <<<"$decision")
  lease_file=$(headers_file job-lease \
    "X-Lease-Token: $(jq -r .lease.token <<<"$job")" \
    "X-Lease-Generation: $(jq -r .lease.generation <<<"$job")")
  api decider POST "$BASE/jobs/$(jq -r .job_id <<<"$job")/completion" "$(jq -nc --arg job "$(jq -r .job_id <<<"$job")" \
    --arg document "$document" '{schema_version: "0.2", job_id: $job, document: $document}')" "$lease_file" >/dev/null
  log "  #$number decided automatically by $(service_name decider)"
}

# raise_concerns: the scenario's concerns about the plans (concerns:), each
# raised by its author, naming the unit and attempt it comes from
# (from:); a dismissed one is then dismissed by a researcher with a reason.
raise_concerns() {
  local concern front number raised
  while read -r concern; do
    front="kind: $(jq -r .kind <<<"$concern")"
    if jq -e .from <<<"$concern" >/dev/null; then
      number=${NUMBER[$(jq -r .from.unit <<<"$concern")]}
      front+=$'\n'"unit: $number"
      if jq -e .from.attempt <<<"$concern" >/dev/null; then
        front+=$'\n'"attempt: $(jq -r .from.attempt <<<"$concern")"
      fi
    fi
    raised=$(api "$(jq -r .by <<<"$concern")" POST "$BASE/tracks/$(jq -r .track <<<"$concern")/concerns" \
      "$(jq -nc --arg document "$(printf -- '---\n%s\n---\n%s' "$front" "$(jq -r .body <<<"$concern")")" '{document: $document}')")
    log "concern about the $(jq -r .track <<<"$concern") plan raised by $(jq -r .by <<<"$concern")"
    if jq -e .dismissed <<<"$concern" >/dev/null; then
      api "$(jq -r .dismissed.by <<<"$concern")" POST "$BASE/concerns/$(jq -r .id <<<"$raised")/dismissal" \
        "$(jq -c '{reason: .dismissed.reason}' <<<"$concern")" >/dev/null
      log "  dismissed by $(jq -r .dismissed.by <<<"$concern")"
    fi
  done < <(pv '.concerns // [] | .[]')
}

# write_up NUMBER WRITEUP_JSON: the unit's write-up. An agent claims the
# document job and completes it; a researcher writes it up in one action, or
# skips it with a reason (skip:). The front matter cites what the job names.
write_up() {
  local number=$1 writeup=$2 by job lease_file inputs document
  by=$(jq -r .by <<<"$writeup")
  if jq -e .skip <<<"$writeup" >/dev/null; then
    api "$by" POST "$BASE/units/$number/writeup/skip" "$(jq -c '{reason: .skip}' <<<"$writeup")" >/dev/null
    log "  #$number write-up skipped by $by"
    return
  fi
  if [[ "$by" == agent ]]; then
    job=$(api "$by" POST "$BASE/jobs/claims" '{"phase":"document"}' | jq -c .job)
    [[ "$(jq -r .unit <<<"$job")" == "$number" ]] ||
      die "$by claimed the document job of another unit; only the first write-up of a project is an agent's"
    inputs=$(jq -c .inputs <<<"$job")
  else
    inputs=$(api "$by" GET "$BASE/units/$number/writeup" | jq -c .inputs)
  fi
  document=$(jq -r --argjson inputs "$inputs" '"---\nsummary: \(.summary | tojson)\nattempts: \($inputs.attempts | tojson)\nverification: \($inputs.verification // null | tojson)\n---\n\n\(.body)\n"' <<<"$writeup")
  if [[ "$by" == agent ]]; then
    lease_file=$(headers_file job-lease \
      "X-Lease-Token: $(jq -r .lease.token <<<"$job")" \
      "X-Lease-Generation: $(jq -r .lease.generation <<<"$job")")
    api "$by" POST "$BASE/jobs/$(jq -r .job_id <<<"$job")/completion" "$(jq -nc --arg job "$(jq -r .job_id <<<"$job")" \
      --arg document "$document" '{schema_version: "0.2", job_id: $job, document: $document}')" "$lease_file" >/dev/null
  else
    api "$by" POST "$BASE/units/$number/writeup" "$(jq -nc --arg document "$document" '{document: $document}')" >/dev/null
  fi
  log "  #$number written up by $by"
}

# finish NUMBER SPEC: after an attempt is verified or stopped, its write-up
# (writeup:) and the decision (decision:), as far as the scenario goes.
finish() {
  local number=$1 spec=$2
  jq -e .writeup <<<"$spec" >/dev/null || return 0
  write_up "$number" "$(jq -c .writeup <<<"$spec")"
  if jq -e .decision <<<"$spec" >/dev/null; then
    decide "$number" decision "$(jq -c .decision <<<"$spec")"
  fi
}

comments() {
  local i=$1 key number comment on target created
  key=$(hvr "$i" .key)
  number=${NUMBER[$key]-}
  [[ -n "$number" ]] || return 0
  while read -r comment; do
    on=$(jq -r .on <<<"$comment")
    if [[ "$on" == unit ]]; then
      target=$BASE/units/$number/comments
    else
      target=$BASE/units/$number/attempts/$on/comments
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
  count=$(pv '.units | length')
  # Plans: each unit is a unit of its track's approved plan, written once
  # what its relations name exists; later units wait for the end.
  plan_all "$count"
  # Attempts. A claim of a verify job cannot name its attempt, so units
  # that leave a job open go last: verified work first, then the verify jobs
  # left claimed, then those left waiting, then the running attempt.
  order=$(pv '[.units | to_entries[] | select(.value.attempts)
    | {i: .key, g: ({"verifying": 1, "submitted": 2, "running": 3}[.value.attempts[-1].stop] // 0)}]
    | sort_by(.g, .i) | .[].i')
  for i in $order; do
    attempts=$(hv "$i" '.attempts | length')
    log "#${NUMBER[$(hvr "$i" .key)]} attempts"
    for ((a = 0; a < attempts; a++)); do run_attempt "$i" "$a"; done
  done
  for ((i = 0; i < count; i++)); do comments "$i"; done
  later_plans "$count"
  # After the plans: a draft cannot be submitted while a concern waits for an answer.
  raise_concerns
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
