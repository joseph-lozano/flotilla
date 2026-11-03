#!/bin/sh
# A stand-in for the GitHub CLI, for screenshots of Flotilla's GitHub states.
# FAKE_GH_MODE: old (gh too old), signed-out (no sign-in). Point FLOTILLA_SNAPSHOT_GH here.
case "$1" in
  --version)
    if [ "$FAKE_GH_MODE" = old ]; then echo "gh version 2.40.1 (2023-12-13)"; else echo "gh version 2.100.0 (2026-09-30)"; fi ;;
  auth)
    if [ "$2" = status ]; then echo '{"hosts":{}}'; exit 1; fi
    if [ "$2" = token ]; then exit 1; fi ;;
esac
