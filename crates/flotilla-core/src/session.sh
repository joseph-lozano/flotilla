# Flotilla: the harness session behind boat conversation $CONV, for the scripts put after this
# one (lineage.sh, meter.sh). `find_session` sets T (the harness: codex, claude-code or
# opencode) and SID (its session's id), and for Codex FILES (every rollout) and for OpenCode DB;
# when there's none it calls `none`, which each script defines to print its empty answer.
# During a conversation's first turn boat hasn't noted the harness session yet (on a new VM
# it has no conversation file at all), only the turn in flight. Then the session is the
# newest top-level one written lately that has that turn's prompt, or failing that just the
# newest (a Flotilla thread's VM runs one conversation).
safe() { case "$1" in ''|*[!0-9A-Za-z_-]*) return 1;; esac; }
find_session() {
command -v jq >/dev/null || none
safe "$CONV" || none
S="$HOME/.ascii/conversation-state.json"
I="$HOME/.ascii/turns-in-flight.json"
T=$(jq -r --arg c "$CONV" '.conversations[$c].adapterState.adapterType // empty' "$S" 2>/dev/null)
[ -n "$T" ] || T=$(jq -r --arg c "$CONV" '.turns[$c].agentType // empty' "$I" 2>/dev/null)
SID=$(jq -r --arg c "$CONV" '.conversations[$c].adapterState.sessionId // empty' "$S" 2>/dev/null)
PROMPT=$(jq -r --arg c "$CONV" '.turns[$c].prompt // empty' "$I" 2>/dev/null | head -1 | cut -c1-60)
case "$T" in
codex)
  # Codex keeps its databases locked while a turn runs; its session files ("rollouts") are
  # written as it goes.
  R="$HOME/.codex/sessions"
  [ -d "$R" ] || none
  FILES=$(find "$R" -name 'rollout-*.jsonl' 2>/dev/null)
  [ -n "$FILES" ] || none
  if ! safe "$SID"; then
    NEW=$(find "$R" -name 'rollout-*.jsonl' -mmin -30 2>/dev/null)
    [ -n "$NEW" ] || none
    SID=""
    for F in $( { [ -n "$PROMPT" ] && grep -l -F -- "$PROMPT" $NEW 2>/dev/null | xargs -r ls -t 2>/dev/null; ls -t $NEW 2>/dev/null; } ); do
      [ -f "$F" ] || continue
      SID=$(head -1 "$F" | jq -r 'select(.payload.parent_thread_id == null) | .payload.id // empty' 2>/dev/null)
      [ -n "$SID" ] && break
    done
    safe "$SID" || none
  fi
  ;;
claude-code)
  if ! safe "$SID"; then
    NEW=$(find "$HOME/.claude/projects" -mindepth 2 -maxdepth 2 -name '*.jsonl' -mmin -30 2>/dev/null)
    [ -n "$NEW" ] || none
    F=$( { [ -n "$PROMPT" ] && grep -l -F -- "$PROMPT" $NEW 2>/dev/null | xargs -r ls -t 2>/dev/null; ls -t $NEW 2>/dev/null; } | head -1)
    [ -f "$F" ] || none
    SID=$(basename "$F" .jsonl)
    safe "$SID" || none
  fi
  ;;
opencode)
  DB="$HOME/.local/share/opencode/opencode.db"
  [ -f "$DB" ] && command -v sqlite3 >/dev/null || none
  if ! safe "$SID"; then
    P=$(printf %s "$PROMPT" | sed "s/'/''/g")
    LATELY="s.parent_id is null and s.time_updated > (strftime('%s', 'now') - 1800) * 1000"
    SID=$(sqlite3 "$DB" "select s.id from session s join part p on p.session_id = s.id where $LATELY and '$P' != '' and instr(json_extract(p.data, '\$.text'), '$P') > 0 order by s.time_updated desc limit 1" 2>/dev/null)
    safe "$SID" || SID=$(sqlite3 "$DB" "select s.id from session s where $LATELY order by s.time_updated desc limit 1" 2>/dev/null)
    safe "$SID" || none
  fi
  ;;
*) none ;;
esac
}
