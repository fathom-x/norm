#!/bin/bash
# The program the sandbox's terminal runs (the broker starts it in a PTY):
# norm in the demo workspace, started again whenever it exits, so a visitor
# who quits norm is never left at a dead terminal.
#
# NORM_HOME puts everything norm and owallet keep under one directory
# (wallet, auth.json, sessions); the template installs both binaries into
# $NORM_HOME/bin. OWALLET_PASSWORD (set per sandbox by the broker) skips
# every password prompt. NORM_BIN overrides the norm to run (the local
# provider's tests run norm from source); without NORM_WORKSPACE it runs in
# the current directory.
export NORM_HOME="${NORM_HOME:-/home/user/.norm}"
cd "${NORM_WORKSPACE:-$PWD}" || exit 1
while true; do
  ${NORM_BIN:-"$NORM_HOME/bin/norm"}
  printf '\r\n\033[2mnorm exited. Press Enter to start it again.\033[0m'
  read -r _ || exit 0
done
