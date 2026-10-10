#!/bin/sh
# Mounted in the pinned utility image, never executed in the distroless app.
# No credentials in arguments, stdout or diagnostics. Same-directory rename
# ensures readers observe a complete owner-only file after each rotation.
set -eu
umask 077
copy_private() {
    source_file=$1
    destination=$2
    temporary="${destination}.new"
    trap 'rm -f /run/cannery/*.new' EXIT
    trap 'exit 0' HUP INT TERM
    test -s "$source_file"
    cp "$source_file" "$temporary" 2>/dev/null
    chmod 0600 "$temporary" 2>/dev/null
    mv -f "$temporary" "$destination" 2>/dev/null
}
refresh() {
    copy_private /secret/token /run/cannery/verifier.token
    copy_private /identity/token /run/cannery/kubernetes.token
    copy_private /identity/ca.crt /run/cannery/kubernetes-ca.crt
}
refresh
if [ "${1-}" = --loop ]; then
    while :; do
        sleep 15
        refresh
    done
fi
