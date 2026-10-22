#!/bin/bash
# Test-only `ssh` for Target.ssh: real ssh through an already-authenticated ControlMaster socket, so no keys
# are needed. CK_SSH_SOCKET is required.
exec /usr/bin/ssh -S "${CK_SSH_SOCKET:?set CK_SSH_SOCKET}" "$@"
