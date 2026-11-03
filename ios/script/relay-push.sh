#!/usr/bin/env bash
# For RelayPushUITests: once the test writes $READY, send a note about $THREAD the way the
# server would, through the local relay (relay/script/local, with FAKE_APNS_OUT), and deliver
# what the fake APNs got to the simulator. With `--unopened`, deliver the relay's neutral alert
# with ciphertext sealed by a key the phone doesn't have.
#
# Delivered how: with APNS_KEY_FILE (an AuthKey_<KEY ID>.p8, with APNS_KEY_ID and APNS_TEAM_ID),
# through Apple's sandbox to the simulator's real token, as the deployed relay would; that's the
# only way the simulator runs the Notification Service Extension. Without, `xcrun simctl push`,
# which shows the neutral alert as it came (the simulator never runs an extension for it).
#   ios/script/relay-push.sh <sim> <ready file> <data dir> <thread> <fake APNs out dir> [--unopened]
set -euo pipefail
SIM=$1 READY=$2 DATA=$3 THREAD=$4 OUT=$5
cd "$(dirname "$0")/../.."
rm -f "$READY"
while [ ! -f "$READY" ]; do sleep 0.5; done
sleep 1
PUSH=$(mktemp /tmp/flotilla-relay-push.XXXXXX)
if [ "${6:-}" = "--unopened" ]; then
  E=$(node -e 'const c=require("node:crypto");const k=c.randomBytes(32),n=c.randomBytes(12);const x=c.createCipheriv("chacha20-poly1305",k,n,{authTagLength:16});const t=Buffer.concat([x.update("{}"),x.final()]);console.log(Buffer.concat([n,t,x.getAuthTag()]).toString("base64"))')
  printf '{"aps":{"alert":{"title":"Flotilla","body":"New activity"},"mutable-content":1,"sound":"default"},"e":"%s","Simulator Target Bundle":"com.useflotilla.flotilla"}' "$E" > "$PUSH"
else
  FLOTILLA_PUSH_DB="$DATA/flotilla.db" FLOTILLA_PUSH_THREAD="$THREAD" FLOTILLA_RELAY_E2E="${RELAY:-http://127.0.0.1:8796}" \
    FAKE_APNS_OUT="$OUT" FLOTILLA_PUSH_OUT="$PUSH" cargo test -q -p flotilla-core relay_push_for_simctl -- --ignored >/dev/null
fi
if [ -n "${APNS_KEY_FILE:-}" ]; then
  FLOTILLA_APNS_KEY_FILE="$APNS_KEY_FILE" FLOTILLA_APNS_KEY_ID="$APNS_KEY_ID" FLOTILLA_APNS_TEAM_ID="$APNS_TEAM_ID" \
    FLOTILLA_PUSH_DB="$DATA/flotilla.db" FLOTILLA_PUSH_FILE="$PUSH" \
    cargo test -q -p flotilla-core relayed_through_apns_sandbox -- --ignored >/dev/null
  echo "delivered through APNs' sandbox"
else
  xcrun simctl push "$SIM" com.useflotilla.flotilla "$PUSH"
fi
cp "$PUSH" "${READY}.sent.json"
