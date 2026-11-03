#!/bin/bash
# What a composer can offer after / $ or @, read in a thread's VM (found by probing boat's
# harnesses). JSON lines, one per skill or command a harness run by boat can use:
#   {"kind":"skill|command","name","description","harness","dir","path","insert","shadowed"}
# `insert` is the text that invokes it: at the very start of a prompt for Claude Code and
# OpenCode, anywhere for Codex. `shadowed` marks a later file with a name the harness already
# has (Claude Code and OpenCode use the first; Codex sees both, so its `insert` names the file).
# Claude Code names a skill by its folder, Codex and OpenCode by its front matter. Then a last
# line with the repo's files (tracked, or new and not ignored), capped at $MAX_FILES:
#   {"kind":"files","repo","truncated","files":[…]}
# Claude Code's /clear is left out: it starts a new session the thread still shows as one.
set -u
H=${HOME:-/home/user}
CAP=${MAX_FILES:-20000}
# $REPO: the project's folder under ~; otherwise the only git repo directly under ~.
REPO=${REPO:+$H/$REPO}
[ -n "$REPO" ] && [ ! -d "$REPO/.git" ] && REPO=
if [ -z "$REPO" ]; then
  for g in "$H"/*/.git; do [ -e "$g" ] && { [ -z "$REPO" ] && REPO=${g%/.git} || REPO=MANY; }; done
  [ "$REPO" = MANY ] && REPO=
fi
shopt -s nullglob dotglob globstar

# Manifest: harness <TAB> kind <TAB> dir <TAB> file <TAB> rel (path under dir, for names)
# In each harness's lookup order; later duplicates are shadowed.
emit() { # harness kind dir glob…
  local h=$1 k=$2 d=$3; shift 3
  [ -d "$d" ] || return 0
  local f
  for f in "$@"; do printf '%s\t%s\t%s\t%s\t%s\n' "$h" "$k" "$d" "$f" "${f#"$d"/}"; done
}
manifest() {
  # Claude Code (cwd = repo): user beats project; skills are <dir>/<name>/SKILL.md, one level.
  emit claude-code skill   "$H/.claude/skills"      "$H"/.claude/skills/*/SKILL.md
  [ -n "$REPO" ] && emit claude-code skill "$REPO/.claude/skills" "$REPO"/.claude/skills/*/SKILL.md
  emit claude-code command "$H/.claude/commands"    "$H"/.claude/commands/**/*.md
  [ -n "$REPO" ] && emit claude-code command "$REPO/.claude/commands" "$REPO"/.claude/commands/**/*.md
  # Codex (cwd = $HOME): $CODEX_HOME/skills (incl. .system) and ~/.agents/skills. Never the repo's.
  # ~/.codex/prompts is not listed: app-server never expands /prompts:… (TUI only).
  emit codex skill "$H/.codex/skills"   "$H"/.codex/skills/**/SKILL.md
  emit codex skill "$H/.agents/skills"  "$H"/.agents/skills/**/SKILL.md
  # OpenCode (cwd = $HOME): global config, cwd-level .opencode, and Claude's/agents' user folders.
  local d
  for d in "$H/.config/opencode/skill" "$H/.config/opencode/skills" "$H/.opencode/skill" "$H/.opencode/skills" \
           "$H/.claude/skills" "$H/.agents/skills"; do
    emit opencode skill "$d" "$d"/**/SKILL.md
  done
  for d in "$H/.config/opencode/command" "$H/.config/opencode/commands" "$H/.opencode/command" "$H/.opencode/commands"; do
    emit opencode command "$d" "$d"/**/*.md
  done
}

{
manifest | awk -F'\t' '
function jesc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/\t/, " ", s); gsub(/\r/, "", s);
  gsub(/[\001-\037]/, " ", s); return "\"" s "\"" }
function unq(s,  q) { sub(/^[ \t]+/, "", s); sub(/[ \t]+$/, "", s); q = substr(s, 1, 1)
  if (length(s) >= 2 && (q == "\"" || q == SQ) && substr(s, length(s)) == q) s = substr(s, 2, length(s) - 2)
  return s }
BEGIN { SQ = sprintf("%c", 39) }
{
  h = $1; k = $2; d = $3; f = $4; rel = $5
  name = ""; desc = ""; body1 = ""; hidden = 0; state = 0; block = ""; n = 0
  while ((getline line < f) > 0) {
    n++
    if (n == 1 && line ~ /^---[ \t]*$/) { state = 1; continue }
    if (state == 1) {
      if (line ~ /^---[ \t]*$/) { state = 2; continue }
      if (block != "" && line ~ /^[ \t]+[^ \t]/) {            # folded/literal block continuation
        sub(/^[ \t]+/, "", line); desc = (desc == "" ? line : desc " " line); continue }
      block = ""
      if (match(line, /^name:[ \t]*/))        name = unq(substr(line, RLENGTH + 1))
      else if (match(line, /^description:[ \t]*/)) {
        v = substr(line, RLENGTH + 1)
        if (v ~ /^[>|][-+]?[ \t]*$/) { block = "desc"; desc = "" } else desc = unq(v)
      }
      else if (line ~ /^user-invocable:[ \t]*false/) hidden = 1
      continue
    }
    if (body1 == "" && line !~ /^[ \t]*$/ && line !~ /^#/) { body1 = line; if (state != 1) break }
  }
  close(f)
  if (h == "claude-code" && hidden) next
  if (k == "skill") {
    folder = rel; sub(/\/SKILL\.md$/, "", folder); sub(/.*\//, "", folder)
    # Claude names a skill by its folder; Codex and OpenCode by the frontmatter name.
    nm = (h == "claude-code" || name == "") ? folder : name
  } else {
    nm = rel; sub(/\.md$/, "", nm)
    if (h == "claude-code") gsub(/\//, ":", nm)               # Claude namespaces subfolders as a:b
  }
  if (desc == "" && k == "command") desc = body1
  i = ++cnt; H[i] = h; K[i] = k; D[i] = d; F[i] = f; N[i] = nm; DS[i] = desc
  key = h SUBSEP nm; seen[key]++; S[i] = (seen[key] > 1)
}
END {
  for (i = 1; i <= cnt; i++) {
    h = H[i]; nm = N[i]
    if (h == "codex") ins = (seen[h SUBSEP nm] > 1) ? "[$" nm "](" F[i] ")" : "$" nm
    else ins = "/" nm
    printf "{\"kind\":%s,\"name\":%s,\"description\":%s,\"harness\":%s,\"dir\":%s,\"path\":%s,\"insert\":%s,\"shadowed\":%s}\n",
      jesc(K[i]), jesc(nm), jesc(DS[i]), jesc(h), jesc(D[i]), jesc(F[i]), jesc(ins), (h != "codex" && S[i]) ? "true" : "false"
  }
}'

# Built-ins that are not files. Verified under boat prompt: Claude /compact, /clear; OpenCode /init,
# /compact. Claude's bundled skills (/init, /code-review, …) come from its newest transcript when
# there is one, since they live inside the binary.
printf '%s\n' \
 '{"kind":"command","name":"compact","description":"Summarise the conversation to free context","harness":"claude-code","dir":"(built-in)","path":null,"insert":"/compact","shadowed":false}' \
 '{"kind":"command","name":"init","description":"Create or update AGENTS.md for this project","harness":"opencode","dir":"(built-in)","path":null,"insert":"/init","shadowed":false}' \
 '{"kind":"command","name":"compact","description":"Summarise the conversation to free context","harness":"opencode","dir":"(built-in)","path":null,"insert":"/compact","shadowed":false}' \
 '{"kind":"skill","name":"customize-opencode","description":"Edit or create opencode configuration, agents, skills, plugins, MCP servers or permissions","harness":"opencode","dir":"(built-in)","path":null,"insert":"/customize-opencode","shadowed":false}'
if [ -n "$REPO" ]; then
  pdir="$H/.claude/projects/$(printf '%s' "$REPO" | sed 's#[^A-Za-z0-9]#-#g')"
  ts=("$pdir"/*.jsonl)
  if [ ${#ts[@]} -gt 0 ]; then
    t=$(ls -t "${ts[@]}" | head -1)
    grep -m1 '"type":"skill_listing"' "$t" | jq -c '.attachment.content
      | [scan("(?m)^- ([^:\\n ]+)(?: \\([^)\\n]*\\))?: (.*)$")] | .[]
      | {kind:"skill",name:.[0],description:.[1],harness:"claude-code",dir:"(built-in)",path:null,insert:("/"+.[0]),shadowed:false}' 2>/dev/null
  fi
fi
} | jq -c -s 'reduce .[] as $r ({seen:{},out:[]};
      ($r.harness+"\u0000"+$r.name) as $k
      | if $r.dir=="(built-in)" and .seen[$k] then . else .seen[$k]=true | .out+=[$r] end) | .out[]'

# Repo files: tracked + untracked-not-ignored, NUL-safe, capped.
if [ -n "$REPO" ]; then
  git -C "$REPO" ls-files -z --cached --others --exclude-standard 2>/dev/null | head -z -n $((CAP + 1)) \
  | jq -c -R -s --arg repo "$REPO" --argjson cap "$CAP" \
      'split("\u0000") | map(select(. != "")) | unique as $f
       | {kind:"files",repo:$repo,truncated:(($f|length) > $cap),files:$f[0:$cap]}'
else
  printf '{"kind":"files","repo":null,"truncated":false,"files":[]}\n'
fi
