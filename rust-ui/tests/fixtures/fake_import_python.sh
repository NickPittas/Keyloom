#!/bin/sh
# Fake repo python for import UI tests. Committed, never written at test time (writing then exec'ing a script
# races with fork in parallel tests: ETXTBSY); tests symlink `.venv/bin/python` to it. Test state is read from
# the test dir, three levels above the symlink: `calls.log` gets every argv; a `block` file makes it idle until
# stdin closes; otherwise `badproj` args error, dry runs answer one plan, real runs idle.
d=$(dirname "$0")/../../..
echo "$@" >> "$d/calls.log"
[ -e "$d/block" ] && { cat >/dev/null; exit 0; }
case "$*" in
*badproj*) echo '{"event":"error","message":"v1 project"}';;
*--dry-run*) echo '{"event":"plan","items":[{"source":"/s/a","kind":"sequence","status":"ok","clip":"a"}]}';;
*) cat >/dev/null;;
esac
