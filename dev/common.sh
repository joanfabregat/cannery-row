# Shared settings for the dev scripts (sourced, not executed).
# Containers run through run-podman (RUN); private values come from the
# git-ignored dev/local.env (see dev/local.env.example).
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
GROUP=cannery
NODE_IMAGE=docker.io/library/node:22-bookworm-slim
RUN=${RUN:-$HOME/bin/run-podman}

if [[ -f "$REPO/dev/local.env" ]]; then
  # shellcheck source=/dev/null
  source "$REPO/dev/local.env"
fi
# The domain under which run-podman --serve publishes NAME as https://NAME.<domain>.
DEV_DOMAIN=${DEV_DOMAIN:-localhost}
# OIDC client for local sign-in; unset leaves bearer tokens only.
DEV_OIDC_ISSUER=${DEV_OIDC_ISSUER:-}
DEV_OIDC_CLIENT_ID=${DEV_OIDC_CLIENT_ID:-}
DEV_ADMIN_EMAILS=${DEV_ADMIN_EMAILS:-}

# Writes an env file readable only by us under /tmp and prints its path.
# Callers remove it with a trap.
make_env_file() {
  local file
  file=$(umask 077 && mktemp /tmp/cannery-env.XXXXXX)
  printf '%s\n' "$@" >"$file"
  printf '%s\n' "$file"
}

# Prints the dev OIDC client secret by running DEV_OIDC_SECRET_COMMAND, so the
# secret is fetched when needed and never stored in dev/local.env.
oidc_client_secret() {
  [[ -n "${DEV_OIDC_SECRET_COMMAND:-}" ]] || { echo "DEV_OIDC_SECRET_COMMAND is not set in dev/local.env" >&2; return 1; }
  "$DEV_OIDC_SECRET_COMMAND"
}

# Runner-specific locations, overridable in dev/local.env: the Cargo wrapper
# (fetch with the network, build offline), a scratch root and a state root
# the runner can mount.
RUN_RUST=${RUN_RUST:-run-rust}
WORK_ROOT=${WORK_ROOT:-${TMPDIR:-/tmp}}
STATE_ROOT=${STATE_ROOT:-$HOME/.local/state/cannery-row}
