# Flotilla: what the agents in this VM spent, as T3 Code reads it on a Mac: every session
# file the harnesses keep here (Claude Code's projects, its subagents' too; Codex's rollouts;
# OpenCode's database), from where the last read stopped, cut to the fields that count.
#   $CURSOR (base64): one line per file read before, "<id>\t<offset>\t<path>"; id is a
#     checksum of its first bytes, so a file made again (a new VM) is read from its start.
#   $SINCE_S: when the last read that got to the end of everything began (this VM's clock,
#     seconds): a file the cursor doesn't have and that hasn't changed since is passed over.
#   $OC_SINCE, $OC_ID: OpenCode's finished replies after this (time updated, id), in order,
#     $OC_LIMIT at a time.
#   $BUDGET: the most bytes read this time, all files together.
# Prints one JSON object per line: {"now": …} first (this VM's clock); for each file read,
# its path, id, size, harness, the offsets read from and to (whole lines only: a line still
# being written, or one that doesn't fit in what's left of the budget, waits for the next
# read) and its lines that count; {"opencode": …, "full": …} with OpenCode's finished
# replies; and {"more": true} when it stopped short (the rest is read next time).
command -v jq >/dev/null || exit 0
BUDGET=${BUDGET:-8388608}
U=$(mktemp -d 2>/dev/null) || exit 0
trap 'rm -rf -- "$U"' EXIT
if [ -n "${CURSOR:-}" ]; then printf %s "$CURSOR" | base64 -d > "$U/cursor" 2>/dev/null || : > "$U/cursor"; else : > "$U/cursor"; fi
LEFT=$BUDGET
MORE=""
isnum() { case "$1" in ''|*[!0-9]*) return 1;; esac; }
SINCE_S=${SINCE_S:-0}
isnum "$SINCE_S" || SINCE_S=0
echo "{\"now\":$(date +%s)}"
# When a file last changed, its contents or itself (made, copied, moved), in seconds: GNU's
# stat, else BSD's. Unlike its modified time, a copy or move can't carry an old one.
changed() { stat -c %Z "$1" 2>/dev/null || stat -f %c "$1" 2>/dev/null; }
# Claude Code: its replies' usage (and how many of its cache writes are kept an hour), with
# the advisor calls each carries.
CLAUDE='select(.type == "assistant" and (.message.usage | type) == "object") | {timestamp, requestId, sessionId, costUSD, message: {id: .message.id, model: .message.model, usage: (.message.usage | {input_tokens, cache_creation_input_tokens, cache_creation: ((.cache_creation | objects | {ephemeral_1h_input_tokens}) // null), cache_read_input_tokens, output_tokens, speed, iterations: [.iterations[]? | objects | select(.type == "advisor_message") | {type, model, input_tokens, cache_creation_input_tokens, cache_creation: ((.cache_creation | objects | {ephemeral_1h_input_tokens}) // null), cache_read_input_tokens, output_tokens}]})}}'
# Codex: its counts, and what they need from before them (session, model, service tier).
CODEX='{timestamp, type, payload: (.payload | objects | {type, id, session_id, model_provider, turn_id, model, forked_from_id, thread_settings: ((.thread_settings | objects | {service_tier}) // null), source: ((.source | objects | {subagent: ((.subagent | objects | {thread_spawn: ((.thread_spawn | objects | {parent_thread_id}) // null)}) // null)}) // null), info: ((.info | objects | {last_token_usage, total_token_usage}) // null)})}'
read_file() {
  H=$1 F=$2
  case "$F" in *"	"*) return;; esac
  SIZE=$(wc -c < "$F" 2>/dev/null | tr -d ' ')
  isnum "$SIZE" || return
  ID=$(head -c 512 "$F" | cksum | cut -d' ' -f1)
  KEPT=$(awk -F'\t' -v p="$F" '$3 == p { print $1 "\t" $2; exit }' "$U/cursor")
  if [ -z "$KEPT" ] && [ "$SINCE_S" -gt 0 ]; then
    # Read to its end before, and its place since forgotten: only a change brings it back.
    M=$(changed "$F")
    if isnum "$M" && [ "$M" -lt "$SINCE_S" ]; then return; fi
  fi
  FROM=0
  if [ "${KEPT%%	*}" = "$ID" ]; then FROM=${KEPT#*	}; fi
  isnum "$FROM" || FROM=0
  [ "$FROM" -le "$SIZE" ] || FROM=0
  [ "$FROM" -lt "$SIZE" ] || return
  if [ "$LEFT" -le 0 ]; then MORE=1; return; fi
  N=$((SIZE - FROM)) CUT=""
  if [ "$N" -gt "$LEFT" ]; then N=$LEFT CUT=1; fi
  tail -c +$((FROM + 1)) "$F" 2>/dev/null | head -c "$N" > "$U/chunk"
  L=$(wc -c < "$U/chunk" | tr -d ' ')
  if [ "$(tail -c 1 "$U/chunk" | od -An -tx1 | tr -d ' \n')" = 0a ]; then
    USE=$L
  else
    USE=$((L - $(tail -n 1 "$U/chunk" | wc -c | tr -d ' ')))
  fi
  [ -n "$CUT" ] && MORE=1
  if [ "$USE" -le 0 ]; then
    # No whole line in it: still being written, or longer than what's left.
    [ -n "$CUT" ] || return
    if [ "$LEFT" -lt "$BUDGET" ]; then
      # The next read starts with it, with the whole budget.
      LEFT=0
      return
    fi
    # Longer than the whole budget: passed over to its end, once it has one.
    tail -c +$((FROM + 1)) "$F" 2>/dev/null | head -n 1 > "$U/chunk"
    [ "$(tail -c 1 "$U/chunk" | od -An -tx1 | tr -d ' \n')" = 0a ] || return
    USE=$(wc -c < "$U/chunk" | tr -d ' ')
    LEFT=0
    : > "$U/lines"
  else
    LEFT=$((LEFT - USE))
    case "$H" in
    claude) head -c "$USE" "$U/chunk" | grep -F '"usage"' | jq -Rc "fromjson? // empty | $CLAUDE" > "$U/lines" 2>/dev/null ;;
    codex) head -c "$USE" "$U/chunk" | grep -F -e '"token_count"' -e '"turn_context"' -e '"thread_settings_applied"' -e '"session_meta"' | jq -Rc "fromjson? // empty | $CODEX" > "$U/lines" 2>/dev/null ;;
    esac
  fi
  jq -nc --arg f "$F" --arg id "$ID" --arg h "$H" --argjson size "$SIZE" --argjson from "$FROM" --argjson to $((FROM + USE)) --slurpfile l "$U/lines" '{file: $f, id: $id, size: $size, harness: $h, from: $from, to: $to, lines: $l}'
}
if [ -d "$HOME/.claude/projects" ]; then
  find "$HOME/.claude/projects" -type f -name '*.jsonl' 2>/dev/null > "$U/claude"
  while IFS= read -r F; do read_file claude "$F"; done < "$U/claude"
fi
for D in "$HOME/.codex/sessions" "$HOME/.codex/archived_sessions"; do
  [ -d "$D" ] || continue
  find "$D" -type f -name 'rollout-*.jsonl' 2>/dev/null > "$U/codex"
  while IFS= read -r F; do read_file codex "$F"; done < "$U/codex"
done
DB="$HOME/.local/share/opencode/opencode.db"
SINCE=${OC_SINCE:-0}
isnum "$SINCE" || SINCE=0
AFTER=${OC_ID:-}
case "$AFTER" in *[!0-9A-Za-z_-]*) AFTER="";; esac
LIMIT=${OC_LIMIT:-2000}
isnum "$LIMIT" || LIMIT=2000
if [ -f "$DB" ] && command -v sqlite3 >/dev/null; then
  # Only finished replies (a reply's counts grow until it's done), in order of when they
  # last changed and then id, after the last one read.
  if sqlite3 -json "$DB" "select id, session_id, time_updated as updated, data from message where (time_updated > $SINCE or (time_updated = $SINCE and id > '$AFTER')) and json_extract(data, '\$.role') = 'assistant' and json_extract(data, '\$.time.completed') is not null order by time_updated, id limit $LIMIT" > "$U/oc" 2>/dev/null; then
    # None left is said too: it ends the pass through them.
    [ -s "$U/oc" ] || echo '[]' > "$U/oc"
    FULL=false
    [ "$(jq length "$U/oc" 2>/dev/null)" = "$LIMIT" ] && FULL=true MORE=1
    jq -c --argjson full "$FULL" '{full: $full, opencode: [.[] | {id, session_id, updated, data: (.data | fromjson? // {} | {modelID, providerID, model: ((.model | objects | {id, modelID}) // null), tokens, cost, time})}]}' "$U/oc" 2>/dev/null
  fi
fi
[ -z "$MORE" ] || echo '{"more":true}'
exit 0
