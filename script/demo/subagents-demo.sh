#!/bin/zsh
# The subagents demo: a Claude Code thread with two subagents (pinned while they run, then a
# card each), then Codex and OpenCode, narrated by Marin (script/demo/subagents-demo.tsv) into
# ~/Movies/Flotilla/flotilla-subagents.mp4.
#
#   script/demo/subagents-demo.sh voice     voice the narration in one take (OpenRouter's Marin)
#   script/demo/subagents-demo.sh record [take]   record the takes (or one) into $D/{claude,codex,opencode,close}
#   script/demo/subagents-demo.sh build     narrate and assemble each take, then join them
#
# Recorded as Flotilla Dev (FLOTILLA_DEV=1): its threads' VMs are labelled flotilla=dev, so the
# release app doesn't pick them up. FLOTILLA_DEMO=1 hides the Dev tag. Build first:
#   cargo build --release -p flotilla-app --features snapshot --target-dir target/demo
set -eu
root=${0:A:h:h:h}
D=${FLOTILLA_SUBAGENTS_DIR:-/tmp/flotilla-subagents}
V=$D/voice
APP=$root/target/demo/release/flotilla-app
narrate=(uv run --quiet --python 3.13 --with mlx-audio==0.4.2 --with soundfile python $root/script/demo/narrate.py)
db=$D/demo.db

# The default agent for the next New thread.
agent() { sqlite3 $db "update kv set value='$1' where key='default_provider'; update kv set value='$2' where key='default_model'; update kv set value='' where key='default_effort';" }

run() {
  local dir=$1 script=$2
  rm -rf $dir && mkdir -p $dir
  FLOTILLA_DEV=1 FLOTILLA_DEMO=1 FLOTILLA_DB=$db FLOTILLA_SNAPSHOT_DIR=$dir FLOTILLA_SNAPSHOT_SCRIPT=$script $APP > $dir/run.log 2>&1 || true
  grep -E "timed out|unknown" $dir/run.log || true
}

two="Use your Agent tool to start TWO subagents at the same time, in one message. The first, described as 'Check the system', runs uname -a, then sleep 12, then df -h /, one at a time. The second, described as 'Look around home', runs ls ~, then sleep 16, then whoami, one at a time. Don't run anything yourself. When both return, sum them up in one sentence."

record() {
  mkdir -p $D
  # A scratch database: the release one's settings, no threads or projects (kept for a retake).
  if [[ -z ${1:-} || ! -f $db ]]; then
    rm -f $db $db-wal $db-shm
    sqlite3 "$HOME/Library/Application Support/Flotilla/flotilla.db" ".backup $db"
    sqlite3 $db "update kv set value='' where key='last_project'; delete from projects; delete from setup_runs; delete from threads;"
  fi
  [[ -z ${1:-} || $1 == claude ]] && claude
  [[ -z ${1:-} || $1 == codex ]] && codex
  [[ -z ${1:-} || $1 == opencode ]] && opencode
  [[ -z ${1:-} || $1 == close ]] && close
  return 0
}

# 1. Claude Code: two subagents, pinned while they work, then a card each, one opened. The
# pointer stays on the rows' empty right side, off their words.
claude() {
  agent claude-code claude-haiku-5-5
  run $D/claude "wait:3500;step:new-thread;wait:1200;rec:start:15;caption:Subagents;glide:730,360,600;type:$two;wait:500;glide:1065,426,500;click:1065,426;wait:2500;rec:pause;until:subagents,240;wait:1500;rec:resume;caption:Pinned while they work;glide:640,97,700;wait:5600;caption:Each one, live;step:pinned-open;wait:7600;rec:pause;until:idle,300;wait:2000;rec:resume;caption:A card each;glide:840,290,700;wait:6400;caption:Open a card;glide:840,356,500;step:subagent-open;wait:1500;scroll:730,450,-250;wait:3600;rec:stop"
}

# 2. Codex: the same, from what Codex records (boat shows none of it).
codex() {
  agent codex gpt-5.6-terra
  run $D/codex "wait:3500;step:new-thread;wait:1000;type:${two//Agent tool/spawn_agent tool} Wait for both with wait_agent.;glide:1065,426,500;click:1065,426;wait:2000;until:subagents,240;wait:1500;rec:start:15;caption:Codex too;wait:3200;rec:pause;until:idle,300;wait:3000;rec:resume;glide:840,300,600;wait:4800;rec:stop"
}

# 3. OpenCode: a subagent's card opened, with the task the agent gave it.
opencode() {
  agent opencode gpt-5.6-terra
  run $D/opencode "wait:3500;step:new-thread;wait:1000;type:Use your task tool to start ONE subagent described as 'Check the system'. Have it run uname -a, then df -h /, and report in one sentence. Don't run anything yourself. Then sum up its report in one sentence.;glide:1065,426,500;click:1065,426;wait:2000;until:idle,300;wait:3000;rec:start:15;caption:And OpenCode;glide:840,280,600;step:subagent-open;wait:3900;rec:stop"
}

# 4. The three threads in the sidebar, the first one open (the pointer kept off the rows,
# whose hover cards would cover them).
close() {
  agent claude-code claude-haiku-5-5
  run $D/close "wait:3500;step:thread;wait:1500;rec:start:15;caption:Flotilla;glide:760,560,900;wait:7400;rec:stop"
}

build() {
  for t in claude codex opencode close; do
    $narrate mix $D/$t/captions.tsv $V/narration.tsv $D/$t/narration.wav
    zsh $root/script/demo/assemble.sh $D/$t $D/$t.mp4 $D/$t/narration.wav
  done
  mkdir -p ~/Movies/Flotilla
  local inputs=() filters="" joined="" i=0
  for t in claude codex opencode close; do
    local len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $D/$t.mp4)
    local out=$(python3 -c "print(max(0.0, $len - 0.3))")
    inputs+=(-i $D/$t.mp4)
    filters+="[${i}:v]fade=t=in:st=0:d=0.2,fade=t=out:st=${out}:d=0.3,setpts=PTS-STARTPTS[v${i}];"
    filters+="[${i}:a]afade=t=in:st=0:d=0.1,afade=t=out:st=${out}:d=0.3,asetpts=PTS-STARTPTS[a${i}];"
    joined+="[v${i}][a${i}]"
    i=$((i + 1))
  done
  ffmpeg -hide_banner -loglevel error -y $inputs -filter_complex "${filters}${joined}concat=n=$i:v=1:a=1[v][a]" -map "[v]" -map "[a]" \
    -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart ~/Movies/Flotilla/flotilla-subagents.mp4
  ffprobe -v error -show_entries format=duration -of csv=p=0 ~/Movies/Flotilla/flotilla-subagents.mp4
}

case ${1:-} in
  voice) cd $root && NARRATOR=openrouter $narrate take $root/script/demo/subagents-demo.tsv $V marin ;;
  record) record ${2:-} ;;
  build) build ;;
  *) print -u2 "usage: $0 voice | record | build"; exit 1 ;;
esac
