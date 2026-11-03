#!/bin/zsh
# The hand-offs demo: an agent handing a task to another (each in its own VM) and being woken
# with the answer, the composer's / $ @ menu, attachment tiles, and your instructions and
# skills in every VM, narrated by Marin (script/demo/handoffs-demo.tsv) into
# ~/Movies/Flotilla/flotilla-handoffs.mp4.
#
#   script/demo/handoffs-demo.sh live      start the hand-off for real (a Claude thread asks a
#                                          Codex one), into $D/live.db
#   script/demo/handoffs-demo.sh voice     voice the narration in one take (OpenRouter's Marin)
#   script/demo/handoffs-demo.sh record    record the takes offline, from $D/live.db and
#                                          seeded copies of it
#   script/demo/handoffs-demo.sh build     narrate and assemble each take, then join them
#
# Recorded as Flotilla Dev (FLOTILLA_DEV=1): its VMs are labelled flotilla=dev, so the release
# app doesn't pick them up. FLOTILLA_DEMO=1 hides the Dev tag. Build first:
#   cargo build --release -p flotilla-app --features snapshot --target-dir target/demo
set -eu
root=${0:A:h:h:h}
D=${FLOTILLA_HANDOFFS_DIR:-/tmp/flotilla-handoffs}
V=$D/voice
APP=$root/target/demo/release/flotilla-app
narrate=(uv run --quiet --python 3.13 --with mlx-audio==0.4.2 --with soundfile python $root/script/demo/narrate.py)
TAKES=(handoff menu attach settings close)

# A copy of the live run's database, with `sql` run on it, for one take.
seed() {
  local db=$D/$1.db
  rm -f $db $db-wal $db-shm
  sqlite3 $D/live.db ".backup $db"
  sqlite3 $db "insert or replace into kv(key,value,updated_at) values('appearance','light','x'); ${2:-}"
  print $db
}

offline() {
  local take=$1 db=$2 script=$3
  rm -rf $D/$take && mkdir -p $D/$take
  FLOTILLA_OFFLINE=1 FLOTILLA_DEV=1 FLOTILLA_DEMO=1 FLOTILLA_DB=$db FLOTILLA_SNAPSHOT_DIR=$D/$take FLOTILLA_SNAPSHOT_SCRIPT=$script $APP > $D/$take/run.log 2>&1 || true
  grep -E "timed out|unknown" $D/$take/run.log || true
}

live() {
  mkdir -p $D
  local db=$D/live.db
  rm -f $db $db-wal $db-shm
  sqlite3 "$HOME/Library/Application Support/Flotilla/flotilla.db" ".backup $db"
  sqlite3 $db "update kv set value='' where key='last_project'; delete from projects; delete from setup_runs; delete from threads;
    insert or replace into kv(key,value,updated_at) values('default_provider','claude-code','x'),('default_model','claude-haiku-5-5','x'),('default_effort','','x'),('appearance','light','x');"
  rm -rf $D/live && mkdir -p $D/live
  FLOTILLA_LOG=1 FLOTILLA_DEV=1 FLOTILLA_DEMO=1 FLOTILLA_DB=$db FLOTILLA_SNAPSHOT_DIR=$D/live \
    FLOTILLA_SNAPSHOT_SCRIPT="wait:4000;newthread:Ask a Codex agent to check the kernel version with uname -r, then tell me what it is in one sentence.;wait:25000;until:idle,300;until:woken,900;wait:3000;shot:woken" \
    $APP > $D/live/run.log 2>&1 || true
  grep -E "\[orchestrate\]|timed out" $D/live/run.log || true
}

record() {
  local prompt="Ask a Codex agent to check the kernel version with uname -r, then tell me what it is in one sentence."
  # 1. The hand-off: the prompt typed, then the thread it made, the child, and the answer.
  offline handoff $(seed handoff) "wait:3000;step:new-thread;wait:1200;rec:start:15;caption:Agents hand off work;glide:730,360,600;type:$prompt;wait:900;glide:1065,426,500;wait:700;open:Codex agent;wait:1200;glide:600,210,700;wait:3800;caption:A thread of its own;glide:140,97,700;wait:1800;click:140,97;wait:3200;glide:520,53,600;wait:800;click:520,53;wait:1500;caption:Woken with the answer;glide:600,400,700;wait:5600;rec:stop"
  # 2. The menu, on a thread whose VM listed this repo's skills and files.
  local files=$(git -C $root ls-files | jq -R -s -c 'split("\n") | map(select(length > 0))')
  local mentions=$(jq -n -c --argjson files "$files" '{root: "/home/user/flotilla", files: $files, entries: [
    {kind: "skill", name: "boat", description: "Use the boat CLI to manage sandboxes, ports and previews", harness: "claude-code", insert: "/boat", shadowed: false},
    {kind: "skill", name: "review-pr", description: "Review a pull request for bugs and missing tests", harness: "claude-code", insert: "/review-pr", shadowed: false},
    {kind: "skill", name: "write-tests", description: "Add focused tests for the change you just made", harness: "claude-code", insert: "/write-tests", shadowed: false},
    {kind: "command", name: "compact", description: "Summarise the conversation to free context", harness: "claude-code", insert: "/compact", shadowed: false},
    {kind: "skill", name: "release-notes", description: "Draft release notes from the commits since the last tag", harness: "claude-code", insert: "/release-notes", shadowed: false}]}')
  local db=$(seed menu)
  sqlite3 $db "insert or replace into kv(key,value,updated_at) select 'mentions:' || id, '${mentions//\'/\'\'}', 'x' from threads;"
  offline menu $db "wait:3000;open:Codex agent;wait:1200;rec:start:15;caption:Skills and commands;glide:520,700,600;type:/;wait:2200;key:down;wait:900;key:down;wait:1400;key:escape;wait:400;type:;wait:300;caption:Mention a file;type:Look at @thre;wait:2400;key:down;wait:900;key:enter;wait:2600;rec:stop"
  # 3. Attachments: a few files of each kind.
  local F=$D/files
  rm -rf $F && mkdir -p $F
  cp $D/live/woken.png "$F/Screenshot 2026-10-30 at 11.41.12 PM.png"
  sips -s format jpeg -Z 1600 "/System/Library/Desktop Pictures/iMac Blue.heic" --out $F/photo.jpg > /dev/null
  printf '# Release notes\n\n- Hand-offs\n' > $F/notes.md
  printf 'fn main() {}\n' > $F/main.rs
  head -c 4096 /dev/urandom > $F/firmware.bin
  printf 'a,b\n1,2\n' > $F/data.csv
  local attach=""
  for f in "Screenshot 2026-10-30 at 11.41.12 PM.png" photo.jpg notes.md main.rs data.csv firmware.bin; do attach+="step:attach:$F/$f;wait:700;"; done
  offline attach $(seed attach) "wait:3000;open:Codex agent;wait:1200;rec:start:15;caption:Attachments;glide:520,700,600;${attach}glide:400,600,700;wait:3000;rec:stop"
  # 4. Settings: your instructions and skills.
  local skills=$(jq -n -c --arg h $HOME '[$h + "/.claude/skills/file-pr", $h + "/.claude/skills/break-ui"]')
  offline settings $(seed settings "insert or replace into kv(key,value,updated_at) values('user_instructions','Use pnpm, not npm.
Run the tests before you say you''re done.','x'),('user_skills','$skills','x');") \
    "wait:3000;step:settings;wait:1200;scroll:640,400,-470;wait:600;rec:start:15;caption:Yours, in every VM;glide:600,200,700;wait:2600;glide:560,425,600;wait:600;click:372,425;wait:2400;key:escape;wait:1500;rec:stop"
  # 5. The threads side by side.
  offline close $(seed close) "wait:3000;open:Codex agent;wait:1500;rec:start:15;caption:Flotilla;glide:140,100,700;wait:2400;glide:140,210,600;wait:3200;rec:stop"
}

build() {
  for t in $TAKES; do
    $narrate mix $D/$t/captions.tsv $V/narration.tsv $D/$t/narration.wav
    zsh $root/script/demo/assemble.sh $D/$t $D/$t.mp4 $D/$t/narration.wav
  done
  mkdir -p ~/Movies/Flotilla
  local inputs=() filters="" joined="" i=0
  for t in $TAKES; do
    local len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $D/$t.mp4)
    local out=$(python3 -c "print(max(0.0, $len - 0.3))")
    inputs+=(-i $D/$t.mp4)
    filters+="[${i}:v]fade=t=in:st=0:d=0.2,fade=t=out:st=${out}:d=0.3,setpts=PTS-STARTPTS[v${i}];"
    filters+="[${i}:a]afade=t=in:st=0:d=0.1,afade=t=out:st=${out}:d=0.3,asetpts=PTS-STARTPTS[a${i}];"
    joined+="[v${i}][a${i}]"
    i=$((i + 1))
  done
  ffmpeg -hide_banner -loglevel error -y $inputs -filter_complex "${filters}${joined}concat=n=$i:v=1:a=1[v][a]" -map "[v]" -map "[a]" \
    -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart ~/Movies/Flotilla/flotilla-handoffs.mp4
  ffprobe -v error -show_entries format=duration -of csv=p=0 ~/Movies/Flotilla/flotilla-handoffs.mp4
}

case ${1:-} in
  live) live ;;
  voice) cd $root && NARRATOR=openrouter $narrate take $root/script/demo/handoffs-demo.tsv $V marin ;;
  record) record ;;
  build) build ;;
  *) print -u2 "usage: $0 live | voice | record | build"; exit 1 ;;
esac
