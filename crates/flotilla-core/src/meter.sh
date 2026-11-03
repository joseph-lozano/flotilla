# Flotilla: how full a boat conversation's context is, and its agent account's usage limits,
# from the harness's own records in the VM, in the session `find_session` (session.sh, put
# before this) finds. Prints one JSON object of what it found ({} for nothing):
#   harness; model (as the harness ran it); at (when it recorded the latest reply);
#   usage (that reply's tokens, in the harness's own fields);
#   window (the model's context window): Codex records it; else $WINDOW, the one the server
#     already knows for this model; else, with $PROBE (between turns: asking takes seconds),
#     Claude Code's /context or OpenCode's models list is asked, and window_text is Claude
#     Code's ("1m") or window OpenCode's;
#   rate_limits and rate_limits_at: Codex's limits for the account as of its latest reply;
#   with $LIMITS, the account's limits as its service tells them now (claude_usage,
#   chatgpt_usage), asked with the harness's own login, which never leaves the VM; what names
#   the person is dropped;
#   provider and auth: OpenCode's provider for the model and how it signs in to it.
# Files may be mid-write: lines that don't parse yet are skipped.
none() { echo '{}'; exit 0; }
command -v tac >/dev/null || tac() { tail -r "$@"; }
# A file's last lines, newest first. A line still being written ends with no newline: one is
# added, so it stays a line of its own (unparsable, so skipped) and isn't glued to the one
# before it.
newest() { { tail -n 2000 "$1"; echo; } | tac; }
num() { case "$1" in ''|*[!0-9]*) return 1;; esac; }
model_ok() { case "$1" in ''|*[!0-9A-Za-z._:/@\[\]-]*) return 1;; esac; }
find_session
# A value for jq's --argjson: the JSON given, or null.
j() { if [ -n "$1" ] && printf %s "$1" | jq -e . >/dev/null 2>&1; then printf %s "$1"; else echo null; fi; }
case "$T" in
claude-code)
  F=$(find "$HOME/.claude/projects" -mindepth 2 -maxdepth 2 -name "$SID.jsonl" 2>/dev/null | head -1)
  [ -f "$F" ] || none
  # Its own latest reply: not a subagent's, nor one Claude Code wrote itself.
  LAST=$(newest "$F" | grep -m 40 -F '"type":"assistant"' | jq -Rc 'fromjson? // empty | select(.isSidechain != true and .message.usage != null and .message.model != "<synthetic>") | {at: .timestamp, model: (.requestedModel // .message.model), usage: (.message.usage | {input_tokens, cache_creation_input_tokens, cache_read_input_tokens, output_tokens})}' 2>/dev/null | head -1)
  [ -n "$LAST" ] || none
  RUN=$(printf %s "$LAST" | jq -r '.model // empty')
  WT=""
  if ! num "$WINDOW" && [ -n "$PROBE" ] && model_ok "$RUN" && command -v claude >/dev/null; then
    # On settings of its own, thrown away after: Claude Code rewrites its settings as it runs,
    # and the thread's own (~/.claude.json) may be in use by a turn starting now. Kept nowhere.
    D=$(mktemp -d 2>/dev/null) || D=""
    if [ -n "$D" ]; then
      WT=$(cd "$HOME" && CLAUDE_CONFIG_DIR="$D" claude -p /context --model "$RUN" --no-session-persistence --output-format json 2>/dev/null | jq -r '.result // empty' 2>/dev/null | grep -m1 -o 'Tokens:[^/]*/ *[0-9.]*[kKmM]\?' | sed 's|.*/ *||')
      rm -rf -- "$D"
    fi
  fi
  CU=""
  if [ -n "$LIMITS" ] && [ -n "$CLAUDE_CODE_OAUTH_TOKEN" ] && command -v curl >/dev/null; then
    CU=$(curl -sS -m 8 -f https://api.anthropic.com/api/oauth/usage -H "Authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN" -H "anthropic-beta: oauth-2025-04-20" 2>/dev/null | jq -c '{five_hour, seven_day, seven_day_opus, seven_day_sonnet, limits: [.limits[]? | {kind, group, percent, resets_at, scope: (.scope.model.display_name // null)}]}' 2>/dev/null)
  fi
  jq -nc --argjson last "$LAST" --arg wt "$WT" --arg w "$WINDOW" --argjson cu "$(j "$CU")" '$last + {harness: "claude-code", window: ($w | tonumber? // null), window_text: (if $wt == "" then null else $wt end), claude_usage: $cu}'
  ;;
codex)
  F=$(printf '%s\n' $FILES | grep -F -- "$SID.jsonl" | head -1)
  [ -f "$F" ] || none
  # Its latest token counts, and the latest limits for the account (a count can come without
  # them, or with a limit for one model alone).
  TC=$(newest "$F" | grep -m 20 -F '"type":"token_count"' | jq -Rsc '[split("\n")[] | fromjson? // empty] | {info: ([.[] | select(.payload.info != null)] | first | if . then {at: .timestamp, usage: .payload.info.last_token_usage, window: .payload.info.model_context_window} else null end), limits: ([.[] | select(.payload.rate_limits != null and (.payload.rate_limits.limit_id // "codex") == "codex")] | first | if . then {rate_limits_at: .timestamp, rate_limits: .payload.rate_limits} else null end)}' 2>/dev/null)
  [ -n "$TC" ] || none
  RUN=$(grep -F '"type":"turn_context"' "$F" | tail -n 1 | jq -Rr 'fromjson? // empty | .payload.model // empty' 2>/dev/null)
  CU=""
  A="$HOME/.codex/auth.json"
  if [ -n "$LIMITS" ] && [ -f "$A" ] && command -v curl >/dev/null; then
    AT=$(jq -r '.tokens.access_token // empty' "$A" 2>/dev/null)
    ACC=$(jq -r '.tokens.account_id // empty' "$A" 2>/dev/null)
    if [ -n "$AT" ]; then
      CU=$(curl -sS -m 8 -f https://chatgpt.com/backend-api/wham/usage -H "Authorization: Bearer $AT" -H "ChatGPT-Account-Id: $ACC" -H "User-Agent: codex_cli_rs" 2>/dev/null | jq -c '{plan_type, rate_limit}' 2>/dev/null)
    fi
  fi
  jq -nc --argjson tc "$TC" --arg run "$RUN" --argjson cu "$(j "$CU")" '($tc.info // {}) + ($tc.limits // {}) + {harness: "codex", model: (if $run == "" then null else $run end), chatgpt_usage: $cu}'
  ;;
opencode)
  # Its own latest reply that has counted its tokens (they're 0 while it streams).
  ROW=$(sqlite3 -json "$DB" "select json_extract(data, '\$.modelID') model, json_extract(data, '\$.providerID') provider, json_extract(data, '\$.tokens') tokens, time_created at from message where session_id = '$SID' and json_extract(data, '\$.role') = 'assistant' and (json_extract(data, '\$.tokens.input') > 0 or json_extract(data, '\$.tokens.cache.read') > 0) order by time_created desc limit 1" 2>/dev/null | jq -c '.[0] // empty | .tokens = (.tokens | fromjson? // null)' 2>/dev/null)
  [ -n "$ROW" ] || none
  P=$(printf %s "$ROW" | jq -r '.provider // empty')
  M=$(printf %s "$ROW" | jq -r '.model // empty')
  W=""
  if num "$WINDOW"; then
    W=$WINDOW
  elif [ -n "$PROBE" ] && [ -f "$HOME/.cache/opencode/models.json" ]; then
    W=$(jq -r --arg p "$P" --arg m "$M" '.[$p].models[$m].limit | (.input // .context) // empty' "$HOME/.cache/opencode/models.json" 2>/dev/null)
  fi
  AUTH=$(jq -r --arg p "$P" '.[$p].type // empty' "$HOME/.local/share/opencode/auth.json" 2>/dev/null)
  jq -nc --argjson row "$ROW" --arg w "$W" --arg auth "$AUTH" '{harness: "opencode", model: $row.model, provider: $row.provider, auth: (if $auth == "" then null else $auth end), at: $row.at, usage: $row.tokens, window: ($w | tonumber? // null)}'
  ;;
*) none ;;
esac
