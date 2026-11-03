#!/bin/zsh
# The narrated projects demo (P1–P9), recorded take by take against a scratch DB and the real
# boat account, then joined into ~/Movies/Flotilla/flotilla-projects-demo.mp4.
#
#   script/demo/projects-demo.sh take <1-7>   record one take (each builds on the ones before)
#   script/demo/projects-demo.sh join          voice, mix and join the takes
#
# Before take 1: quit the real Flotilla.app (its sync would import the demo's VMs), build
#   cargo build --release -p flotilla-app --features snapshot --target-dir target/demo
# and make the scratch DB with Haiku as the default model:
#   flotilla --db $D/demo.db envs && sqlite3 $D/demo.db "insert into kv … default_provider/default_model"
# Between takes the story needs the playground repo (acme/flotilla-playground) to move:
#   before take 3, push a change to main that conflicts with the agent's PR (here: cap each wait
#   at maxMs on the same line); take 5 pushes a commit that breaks .agents/setup 12 s in.
# Coordinates are window points in the 1180×780 window, read off dry-run screenshots.
set -eu
root=${0:A:h:h:h}
D=${FLOTILLA_DEMO_DIR:-/tmp/flotilla-demo}
PY=${KOKORO_PYTHON:-$HOME/.cache/flotilla-demo/kokoro/.venv/bin/python}

takes=(
  # 1. Intro, Settings → GitHub, Add project (the list loads while paused), the first setup run.
  "wait:3000;rec:start:15;caption:Flotilla;wait:13200;glide:257,762,700;click:257,762;wait:600;caption:GitHub, through the gh CLI;wait:7600;glide:790,376,600;click:790,376;step:search-repos:playground;rec:pause;until:repos,240;rec:resume;wait:300;caption:Add a project;wait:2500;glide:432,204,600;click:432,204;wait:4400;caption:Setup comes from .agents/setup;wait:8300;glide:827,423,500;click:827,423;wait:3000;step:open-project:flotilla-playground;wait:1000;caption:Setup runs once, threads copy it;wait:9200;rec:pause;until:setup-done,900;rec:resume;wait:800;caption:Ready;wait:6400;rec:stop"
  # 2. A new thread in the project.
  "wait:3500;rec:start:15;caption:New thread;step:project:flotilla-playground;wait:6400;caption:Ask for a change;type:Add jitter to retry's backoff so callers don't all retry at the same moment. Keep the change small, then open a draft pull request.;wait:4500;glide:1065,396,500;click:1065,396;wait:1200;caption:Copy, branch, setup;wait:13500;caption:The agent works in its own VM;wait:12000;rec:stop"
  # 3. The draft PR conflicts with main; Resolve; the agent rebases; checks pass.
  "wait:3500;open:Add jitter;wait:2000;rec:start:15;caption:A draft pull request;glide:640,145,900;wait:10000;glide:880,85,700;caption:Main moved on;wait:8600;glide:975,145,600;click:975,145;wait:900;caption:Resolve;wait:9300;glide:1133,747,600;click:1133,747;wait:5000;rec:pause;until:idle,1200;until:mergeable,600;wait:25000;until:checks-pass,1200;rec:resume;wait:1200;caption:Conflict resolved, checks pass;wait:5800;rec:stop"
  # 4. Ready for review, Merge with its confirmation, setup follows main.
  "wait:3500;open:Add jitter;wait:6000;rec:start:15;caption:Ready for review, then merge;glide:1092,145,700;click:1092,145;wait:5000;glide:1116,145,500;click:1116,145;wait:1600;glide:765,197,500;click:765,197;wait:1000;rec:pause;until:merged,240;rec:resume;wait:1200;caption:Merged, setup follows main;step:open-project:flotilla-playground;rec:pause;until:setup-running,120;rec:resume;wait:5800;rec:pause;until:setup-done,600;rec:resume;wait:2500;rec:stop"
  # 5. Setup breaks on main (pushed 12 s in); Fix in a new thread; it starts anyway.
  "wait:3500;step:open-project:flotilla-playground;wait:1500;rec:start:15;caption:Setup broke on main;wait:7400;rec:pause;until:setup-running,180;rec:resume;wait:2500;rec:pause;until:setup-failed,600;rec:resume;wait:3500;glide:1060,90,600;step:fix-setup:flotilla-playground;wait:600;caption:Fix in a new thread;wait:8000;step:new-thread-submit;wait:1500;caption:Threads start even when setup fails;wait:14000;rec:stop"
  # 6. The branch picker, then New thread on this branch from a thread's menu.
  "wait:3500;step:new-thread;wait:2500;rec:start:15;caption:Start on a branch or pull request;glide:600,435,700;click:600,435;wait:2200;glide:690,517,700;wait:1400;glide:660,544,500;wait:1300;glide:700,489,500;wait:900;key:escape;wait:600;caption:New thread on this branch;glide:140,150,700;rclick:140,150;wait:1200;glide:253,343,500;click:253,343;wait:3800;rec:stop"
  # 7. Settings (branch prefix, idle stop), a hover card and the project filter.
  "wait:3500;rec:start:15;caption:Settings;glide:257,762,600;click:257,762;wait:1200;glide:447,562,600;wait:2000;scroll:590,500,-500;wait:600;glide:410,431,500;wait:2000;key:escape;wait:600;caption:That's projects;glide:140,150,700;wait:4500;glide:110,54,600;click:110,54;wait:2500;glide:28,54,500;click:28,54;wait:1500;rec:stop"
)

case ${1:-} in
  take)
    n=$2
    rm -rf $D/s$n && mkdir -p $D/s$n
    FLOTILLA_DB=$D/demo.db FLOTILLA_SNAPSHOT_DIR=$D/s$n FLOTILLA_SNAPSHOT_SCRIPT=${takes[$n]} $root/target/demo/release/flotilla-app
    ;;
  join)
    $PY $root/script/demo/narrate.py say $root/script/demo/projects-demo.tsv $D/voice af_heart
    inputs=(); filters=""; joined=""
    for n in {1..7}; do
      $PY $root/script/demo/narrate.py mix $D/s$n/captions.tsv $D/voice/narration.tsv $D/s$n/narration.wav
      zsh $root/script/demo/assemble.sh $D/s$n $D/s$n.mp4 $D/s$n/narration.wav >/dev/null
      i=$((n - 1))
      len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $D/s$n.mp4)
      out=$(python3 -c "print(max(0.0, $len - 0.35))")
      inputs+=(-i $D/s$n.mp4)
      # Braces: zsh reads `$i:a` as a path modifier.
      filters+="[${i}:v]fade=t=in:st=0:d=0.25,fade=t=out:st=${out}:d=0.35,setpts=PTS-STARTPTS[v${i}];"
      filters+="[${i}:a]afade=t=in:st=0:d=0.15,afade=t=out:st=${out}:d=0.35,asetpts=PTS-STARTPTS[a${i}];"
      joined+="[v${i}][a${i}]"
    done
    mkdir -p ~/Movies/Flotilla
    ffmpeg -hide_banner -loglevel error -y $inputs -filter_complex "${filters}${joined}concat=n=7:v=1:a=1[v][a]" \
      -map "[v]" -map "[a]" -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart \
      ~/Movies/Flotilla/flotilla-projects-demo.mp4
    ffprobe -v error -show_entries format=duration -of csv=p=0 ~/Movies/Flotilla/flotilla-projects-demo.mp4
    ;;
  *) print -u2 "usage: $0 take <1-7> | join"; exit 1 ;;
esac
