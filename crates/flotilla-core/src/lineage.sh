# Flotilla: a boat conversation's subagents, from the harness's own records in the VM, in the
# session `find_session` (session.sh, put before this) finds. Prints {"subagents":[…]}: each
# one's id, the call that started it (Claude Code), its name, kind and model, whether it's
# running or in the background, when it started and finished (ms), its own tool call ids (as
# boat shows them) and what it said. Files may be mid-write: lines that don't parse yet are
# skipped. Times keep their milliseconds: a subagent's start is weighed against prompt times
# to find its turn.
# For Codex it prints "asks" too: the questions the agent asked with request_user_input_async
# (boat's events show them only as text), each its call id, turn, when (ms), the text boat
# shows for it, the questions as Codex has them ({title, options}), its turn's prompt, and
# whether you wrote to the agent after it.
none() { echo '{"subagents":[]}'; exit 0; }
find_session
case "$T" in
codex)
  # A subagent's file ("rollout") starts with its parent's id, then a copy of the parent's
  # history up to `subagent_history_start_ordinal`, then its own lines.
  ms='capture("^(?<s>[^.Z]+)(?<f>\\.[0-9]+)?Z$") | ((.s + "Z") | fromdateiso8601) * 1000 + (("0" + (.f // ".0")) | tonumber * 1000 | floor)'
  # The session's own file ends in its id; only its lines that ask, and your messages, are
  # read: each question with its turn's prompt (your first message in it) and whether you wrote
  # again after it (a steer, or a later turn), by the file's own order.
  ASKS=$(for F in $FILES; do case "$F" in (*"-$SID.jsonl") grep -F -e '"delivery":"async"' -e '"user.text"' "$F" 2>/dev/null ;; esac; done | jq -Rsc '
    [split("\n")[] | fromjson? // empty] as $all |
    [$all[] | select(.type == "response_item" and .payload.type == "message" and .payload.role == "user"
        and ((.payload.internal_chat_message_metadata_passthrough.content_item_kinds // []) | index("user.text")))
      | {o: (.ordinal // 0), turn: .payload.internal_chat_message_metadata_passthrough.turn_id, text: ([.payload.content[]? | .text? // empty] | join(""))}] as $you |
    [$all[] | select(.type == "event_msg" and .payload.type == "item_completed") | (.ordinal // 0) as $o | .payload as $p | $p.item
      | select(.type == "AgentMessage" and .delivery == "async" and ((.questions // []) | length) > 0)
      | {id, turn: $p.turn_id, at: ($p.completed_at_ms // $p.started_at_ms), text: ([.content[]? | .text? // empty] | join("")),
         questions: [.questions[] | {title, options: (.options // [])}],
         prompt: ([$you[] | select(.turn == $p.turn_id)] | first | .text
           | if . then sub("^\\s*<important-instructions-from-user>\\s*"; "") | sub("\\s*</important-instructions-from-user>\\s*$"; "") else null end),
         followed: ([$you[] | select(.o > $o)] | length > 0)}]' 2>/dev/null)
  case "$ASKS" in \[*) ;; *) ASKS='[]' ;; esac
  for F in $(grep -l -F "\"parent_thread_id\":\"$SID\"" $FILES 2>/dev/null); do
    head -1 "$F" | jq -e --arg p "$SID" '.payload.parent_thread_id == $p' >/dev/null 2>&1 || continue
    jq -Rsc "
      [split(\"\\n\")[] | fromjson? // empty] as \$all |
      \$all[0].payload as \$m | (\$m.subagent_history_start_ordinal // 0) as \$from |
      [\$all[] | select((.ordinal // -1) >= \$from)] as \$own |
      [\$own[] | select(.type == \"event_msg\" and .payload.type == \"task_started\")] as \$starts |
      [\$own[] | select(.type == \"event_msg\" and (.payload.type == \"task_complete\" or .payload.type == \"turn_aborted\"))] as \$ends |
      [\$own[] | select(.type == \"event_msg\" and .payload.type == \"item_completed\") | .payload.item] as \$items |
      {
        id: \$m.id,
        name: ((\$m.agent_path // \"\") | split(\"/\") | last | gsub(\"_\"; \" \") | if . == \"\" then \"Subagent\" else . end),
        kind: \$m.agent_nickname,
        model: ([\$own[] | select(.type == \"turn_context\") | .payload.model | select(. != null)] | last),
        running: ((\$starts | length) == 0 or (\$starts | length) > (\$ends | length)),
        background: false,
        started_ms: (if (\$starts | length) > 0 then (\$starts[0].timestamp | $ms) else null end),
        finished_ms: (if (\$ends | length) > 0 and (\$starts | length) <= (\$ends | length) then (\$ends | last | .timestamp | $ms) else null end),
        calls: [\$items[] | select(.type != \"AgentMessage\" and .type != \"UserMessage\" and .type != \"Reasoning\") | .id | select(. != null)],
        texts: [\$items[] | select(.type == \"AgentMessage\") | [.content[]? | .text? // empty] | join(\"\") | select(. != \"\")]
      }" "$F"
  done | jq -sc --argjson asks "$ASKS" '{subagents: ., asks: $asks}'
  ;;
claude-code)
  D=$(find "$HOME/.claude/projects" -mindepth 2 -maxdepth 2 -type d -name "$SID" 2>/dev/null | head -1)
  [ -n "$D" ] && [ -d "$D/subagents" ] || none
  ms='capture("^(?<s>[^.Z]+)(?<f>\\.[0-9]+)?Z$") | ((.s + "Z") | fromdateiso8601) * 1000 + (("0" + (.f // ".0")) | tonumber * 1000 | floor)'
  for M in "$D"/subagents/agent-*.meta.json; do
    [ -f "$M" ] || continue
    A=${M##*/agent-}; A=${A%.meta.json}; J="$D/subagents/agent-$A.jsonl"
    [ -f "$J" ] || J=/dev/null
    jq -Rsc --arg id "$A" --slurpfile meta "$M" "
      [split(\"\\n\")[] | fromjson? // empty] as \$all |
      (\$meta[0] // {}) as \$m | [\$all[] | select(.type == \"assistant\")] as \$said |
      [\$all[] | .timestamp | select(type == \"string\")] as \$times |
      {
        id: \$id,
        parent_call: \$m.toolUseId,
        name: (\$m.description // \"Subagent\"),
        kind: \$m.agentType,
        model: ([\$said[] | .message.model | select(. != null)] | last),
        running: ((\$m.finished // false) | not),
        background: (\$m.requestShape == \"background\"),
        started_ms: (if (\$times | length) > 0 then (\$times | first | $ms) else null end),
        finished_ms: (if (\$m.finished // false) and (\$times | length) > 0 then (\$times | last | $ms) else null end),
        calls: [\$said[] | .message.content[]? | select(.type == \"tool_use\") | .id],
        texts: [\$said[] | .message.content[]? | select(.type == \"text\") | .text]
      }" "$J"
  done | jq -sc '{subagents: .}'
  ;;
opencode)
  # The parent session's `task` calls (with the id boat shows) name each subagent's session.
  # With the agent's own texts after each task call: boat stamps OpenCode's events by when their
  # message began, so its reply can look older than the subagent it waited for.
  TASKS=$(sqlite3 -json "$DB" "select json_extract(t.data, '\$.callID') call, json_extract(t.data, '\$.state.status') status, json_extract(t.data, '\$.state.input') input, json_extract(t.data, '\$.state.metadata.sessionId') child, json_extract(t.data, '\$.state.time.start') started, json_extract(t.data, '\$.state.time.end') ended, (select json_group_array(json_extract(r.data, '\$.text')) from part r where r.session_id = t.session_id and json_extract(r.data, '\$.type') = 'text' and r.time_created > t.time_created) after from part t where t.session_id = '$SID' and json_extract(t.data, '\$.type') = 'tool' and json_extract(t.data, '\$.tool') = 'task' order by t.time_created" 2>/dev/null)
  echo "${TASKS:-[]}" | jq -c '.[]' | while read -r TASK; do
    K=$(echo "$TASK" | jq -r '.child // empty')
    PARTS="[]"
    if safe "$K"; then
      PARTS=$(sqlite3 -json "$DB" "select json_extract(data, '\$.type') t, json_extract(data, '\$.tool') tool, json_extract(data, '\$.callID') call, json_extract(data, '\$.state.input') input, json_extract(data, '\$.text') text from part where session_id = '$K' order by time_created" 2>/dev/null)
    fi
    jq -nc --argjson task "$TASK" --argjson parts "${PARTS:-[]}" '
      (($task.input // "{}") | fromjson? // {}) as $in |
      [$parts[] | select(.t == "tool")] as $tools |
      {
        id: ($task.child // $task.call),
        parent_call: $task.call,
        name: ($in.description // "Subagent"),
        kind: $in.subagent_type,
        prompt: $in.prompt,
        running: (($task.status // "running") | test("^(completed|error)$") | not),
        background: false,
        started_ms: $task.started,
        finished_ms: $task.ended,
        calls: [$tools[] | .call | select(. != null)],
        latest: ($tools | last | if . then [.tool, ((.input // "{}") | fromjson? // {} | .command // .filePath // .pattern // .url // .description // "")] else null end),
        texts: [$parts[] | select(.t == "text") | .text | select(. != null)] | .[1:],
        replies_after: (($task.after // "[]") | fromjson? // [] | map(select(. != null)))
      }'
  done | jq -sc '{subagents: .}'
  ;;
*) none ;;
esac
