#!/bin/sh
# Fake `ssh` for tests. Committed, never written at test time (writing then exec'ing a script races with
# fork in parallel tests: ETXTBSY). Hands the ssh argv to the worker via env, skips options up to `--`, then
# runs the single remote command with `sh -c` as sshd would.
FAKE_SSH_ARGV=$(printf '%s\n' "$@")
export FAKE_SSH_ARGV
while [ "$1" != "--" ]; do shift; done
shift
exec /bin/sh -c "$1"
