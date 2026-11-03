#!/usr/bin/env bash
# Flotilla's command for the agent in this VM. It starts another agent in this same VM (agent)
# or in a VM of its own (delegate), tells one something (message), stops one (cancel), or asks
# the user a question (ask). It only prints a request line (and keeps a copy under
# ~/.flotilla/requests): Flotilla, following this thread, does it.
set -euo pipefail

# What one ask may hold, as `flotilla_core::asks` checks it.
MAX_QUESTIONS=3 MIN_OPTIONS=2 MAX_OPTIONS=4 MAX_QUESTION=1000 MAX_HEADER=40 MAX_LABEL=120 MAX_DESCRIPTION=300

usage() {
  cat >&2 <<'EOF'
Usage:
  flotilla agent [--agent claude-code|codex|opencode] [--model ID] [--title TITLE] [TASK]
      Another agent in this VM, on these files.
  flotilla delegate [--agent claude-code|codex|opencode] [--model ID] [--title TITLE] [TASK]
      Another agent in a VM of its own.
      For both, the task as arguments, or on standard input (a heredoc).
  flotilla message ID TEXT
  flotilla cancel ID
  flotilla ask [--multi] [--header SHORT] QUESTION OPTION OPTION [OPTION [OPTION]]
      Ask the user one question, with 2 to 4 options, the one you recommend first.
  flotilla ask
      Up to 3 questions, as JSON on standard input (a heredoc):
      {"questions": [{"header": "Short", "question": "…?", "options": ["Yes (recommended)", "No"], "multi": false}]}
      An option can be {"label": "…", "description": "…"}; "multi": true lets the user pick several.
EOF
  exit 2
}

# The questions as the request carries them, from what `ask` was given on standard input: as
# Codex's or Claude Code's ask tools take them, or close to it. {"ok": [...]} or {"error": why}.
read_questions() {
  jq -ac --argjson mq "$MAX_QUESTIONS" --argjson mino "$MIN_OPTIONS" --argjson maxo "$MAX_OPTIONS" \
    --argjson mlen "$MAX_QUESTION" --argjson mh "$MAX_HEADER" --argjson ml "$MAX_LABEL" --argjson md "$MAX_DESCRIPTION" '
    def line: tostring | gsub("\\s+"; " ") | sub("^ "; "") | sub(" $"; "");
    def trimmed: tostring | sub("^\\s+"; "") | sub("\\s+$"; "");
    def opt: if type == "string" then {label: line}
      elif type == "object" then {label: ((.label // .title // .value // "") | line)}
        + (((.description // "") | line) as $d | if $d == "" then {} else {description: $d} end)
      else {label: ""} end;
    def question: if type == "string" then {question: trimmed, options: [], multi: false}
      elif type == "object" then {question: ((.question // .title // .text // "") | trimmed),
          options: ((.options // []) | if type == "array" then map(opt) else [] end),
          multi: ((.multi // .multiSelect // .multi_select // .multiple // false) == true)}
        + (((.header // "") | line) as $h | if $h == "" then {} else {header: $h} end)
      else {question: "", options: [], multi: false} end;
    def problems($q):
      (if ($q | length) < 1 or ($q | length) > $mq then "ask 1 to \($mq) questions at once, not \($q | length)" else empty end),
      ($q | to_entries[] | (.key + 1) as $n | .value |
        (if .question == "" then "question \($n) has no text" else empty end),
        (if (.question | length) > $mlen then "question \($n) is over \($mlen) characters" else empty end),
        (if ((.header // "") | length) > $mh then "the header of question \($n) is over \($mh) characters" else empty end),
        (if (.options | length) < $mino or (.options | length) > $maxo then "question \($n) needs \($mino) to \($maxo) options, not \(.options | length)" else empty end),
        (.options[] | if .label == "" then "an option of question \($n) has no label"
          elif (.label | length) > $ml then "option \"\(.label)\" is over \($ml) characters"
          elif ((.description // "") | length) > $md then "the description of option \"\(.label)\" is over \($md) characters"
          else empty end),
        (.options | map(.label) | group_by(.) | map(select(length > 1))[0][0] // empty | "question \($n) has the option \"\(.)\" twice"));
    (if type == "object" and has("questions") then .questions elif type == "array" then . else [.] end) as $given
    | if ($given | type) != "array" then {error: "give the questions as {\"questions\": [...]}"}
      else ($given | map(question)) as $q | ([problems($q)] | first) as $why
      | if $why then {error: $why} else {ok: $q} end end'
}

new_id() { cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr '[:upper:]' '[:lower:]'; }

# The operands, in `operands`, without the first lone `--`: wherever it comes it ends the
# options, as getopt has it, so `ask "Which mode?" -- --safe --fast` has two options, not three.
# Only the first is dropped: a `--` after it is text.
drop_dashdash() {
  local a seen=false
  operands=()
  for a in "$@"; do
    if ! $seen && [ "$a" = -- ]; then seen=true; else operands+=("$a"); fi
  done
}

sha256() { if command -v sha256sum >/dev/null; then sha256sum; else shasum -a 256; fi | cut -d' ' -f1; }

# HMAC-SHA256 of "$2 $3" with the key in $1 (its 64 hex digits as text, one block long), cut to
# 128 bits, in hex. Plain bash and sha256sum, so it works in any VM.
mac() {
  local key=$1 ipad="" opad="" i c x inner
  for ((i = 0; i < 64; i++)); do
    printf -v c '%d' "'${key:i:1}"
    printf -v x '\\x%02x' $((c ^ 0x36)); ipad+=$x
    printf -v x '\\x%02x' $((c ^ 0x5c)); opad+=$x
  done
  inner=$({ printf "$ipad"; printf '%s %s' "$2" "$3"; } | sha256)
  { printf "$opad"; printf "$(printf '%s' "$inner" | sed 's/../\\x&/g')"; } | sha256 | cut -c1-32
}

# Print the request for Flotilla, signed with this thread's key so Flotilla knows it's from
# this VM (the key itself is never printed), and keep a copy in case the line reaches it cut
# short.
request() {
  local id=$1 json=$2 key line
  key=$(cat "$HOME/.flotilla/key" 2>/dev/null || true)
  if [ ${#key} -ne 64 ]; then echo "flotilla: this VM isn't set up for hand-offs yet: try again after the next message" >&2; exit 1; fi
  line="FLOTILLA $id $(mac "$key" id "$id")$(mac "$key" request "$json") $json"
  mkdir -p "$HOME/.flotilla/requests"
  printf '%s\n' "$line" > "$HOME/.flotilla/requests/$id.line"
  printf '%s\n' "$line"
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
  agent|delegate)
    agent="" model="" title=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --agent) agent=${2:?--agent needs a value}; shift 2 ;;
        --model) model=${2:?--model needs a value}; shift 2 ;;
        --title) title=${2:?--title needs a value}; shift 2 ;;
        --) break ;;
        -h|--help) usage ;;
        # Only what looks like an option is one: "- check the docs" is the task.
        --[A-Za-z]*|-[A-Za-z]) case "$1" in *[!A-Za-z0-9-]*) break ;; esac; echo "flotilla: unknown option $1" >&2; usage ;;
        *) break ;;
      esac
    done
    drop_dashdash "$@"; set -- ${operands[@]+"${operands[@]}"}
    case "$agent" in ""|claude-code|codex|opencode) ;; *) echo "flotilla: --agent is claude-code, codex or opencode" >&2; exit 2 ;; esac
    if [ $# -gt 0 ]; then task="$*"; else task=$(cat); fi
    if [ -z "${task//[[:space:]]/}" ]; then echo "flotilla: no task given" >&2; exit 2; fi
    id=$(new_id)
    request "$id" "$(jq -acn --arg op "$cmd" --arg id "$id" --arg agent "$agent" --arg model "$model" --arg title "$title" --arg task "$task" \
      '{op: $op, id: $id, agent: $agent, model: $model, title: $title, task: $task}')"
    if [ "$cmd" = agent ]; then
      echo "Started $id in this VM. It answers with the other agents you start this turn, in one message starting with [flotilla] once all of them have, and you get a new turn then. Start any others now; don't wait or poll: do other work, or end your turn. Tell the user what you handed off, not its id."
    else
      echo "Delegated as $id: it runs in a VM of its own. When it finishes, Flotilla puts its answer here as a new message starting with [flotilla], and you get a new turn then. Don't wait or poll: do other work, or end your turn now with one line on what you handed off (not its id)."
    fi
    ;;
  message)
    drop_dashdash "$@"; set -- ${operands[@]+"${operands[@]}"}
    [ $# -ge 2 ] || usage
    to=$1; shift
    id=$(new_id)
    request "$id" "$(jq -acn --arg id "$id" --arg to "$to" --arg text "$*" '{op: "message", id: $id, to: $to, text: $text}')"
    echo "Sent to $to. Its answer will arrive here as a new message starting with [flotilla]."
    ;;
  cancel)
    drop_dashdash "$@"; set -- ${operands[@]+"${operands[@]}"}
    [ $# -eq 1 ] || usage
    id=$(new_id)
    request "$id" "$(jq -acn --arg id "$id" --arg to "$1" '{op: "cancel", id: $id, to: $to}')"
    echo "Stopping $1."
    ;;
  ask)
    multi=false header=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --multi) multi=true; shift ;;
        --header) header=${2:?--header needs a value}; shift 2 ;;
        --) break ;;
        -h|--help) usage ;;
        # Only what looks like an option is one: "-v or -q?" is a question.
        --[A-Za-z]*|-[A-Za-z]) case "$1" in *[!A-Za-z0-9-]*) break ;; esac; echo "flotilla: unknown option $1" >&2; usage ;;
        *) break ;;
      esac
    done
    drop_dashdash "$@"; set -- ${operands[@]+"${operands[@]}"}
    if [ $# -gt 0 ]; then
      # `--`: what follows is text for jq too, however it starts ("--safe" as an option).
      given=$(jq -acn --arg header "$header" --argjson multi "$multi" '{header: $header, question: $ARGS.positional[0], options: $ARGS.positional[1:], multi: $multi}' --args -- "$@")
    else
      given=$(cat)
    fi
    if [ -z "${given//[[:space:]]/}" ]; then echo "flotilla: no question given" >&2; usage; fi
    if ! parsed=$(printf '%s' "$given" | read_questions 2>/dev/null | head -n 1) || [ -z "$parsed" ]; then
      echo "flotilla: what's on standard input isn't JSON: see flotilla ask --help" >&2
      exit 2
    fi
    why=$(printf '%s' "$parsed" | jq -r '.error // empty')
    if [ -n "$why" ]; then echo "flotilla: $why" >&2; exit 2; fi
    id=$(new_id)
    request "$id" "$(printf '%s' "$parsed" | jq -ac --arg id "$id" '{op: "ask", id: $id, questions: .ok}')"
    echo "Asked: the user sees your question and its options in this conversation, and on their phone. End your turn now. Don't repeat or reword the question or its options in your reply (they'd see it twice): a few words at most, like \"Waiting for your answer.\" Don't answer it yourself or go on as if they had: their answer comes as your next message."
    ;;
  *) usage ;;
esac
