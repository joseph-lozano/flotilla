#!/bin/zsh
# The complete Flotilla demo (design/demo-plan.md), recorded take by take against a scratch DB
# and the real boat account, narrated by Holler's Kit (script/demo/full-demo.tsv).
#
#   script/demo/full-demo.sh seed          start the finished threads chapter 1 shows (not recorded)
#   script/demo/full-demo.sh take <NN>     record one chapter into $D/chNN (they build on each other)
#   script/demo/full-demo.sh voice         voice the narration (cached; only changed lines redo)
#   script/demo/full-demo.sh mix <NN>      narrate and assemble $D/chNN.mp4
#
# Before: quit the real Flotilla.app (its sync would import the demo's VMs, and the demo's would
# import yours: archive any of your threads that appear, in the scratch DB only), build
#   cargo build --release -p flotilla-app --features snapshot --target-dir target/demo
# and make the scratch DB with Haiku as the default model:
#   flotilla --db $D/demo.db envs && sqlite3 $D/demo.db "insert … default_provider/default_model"
# Coordinates are window points in the 1180×780 window, read off dry-run screenshots
# (FLOTILLA_OFFLINE=1 on a copy of the DB, so nothing gets marked read).
set -eu
root=${0:A:h:h:h}
D=${FLOTILLA_DEMO_DIR:-/tmp/flotilla-video}
# Narration: Holler Kit in $D/voice by default; e.g. NARRATOR=openrouter VOICE_DIR=$D/voice-marin.
V=${VOICE_DIR:-$D/voice}
APP=$root/target/demo/release/flotilla-app
narrate=(uv run --quiet --python 3.13 --with mlx-audio==0.4.2 --with soundfile python $root/script/demo/narrate.py)

# The newest thread's title (for open: in a later take).
newest() { sqlite3 $D/demo.db "select title from threads order by created_at desc limit 1" | cut -c1-24 }
title_of() { sqlite3 $D/demo.db "select title from threads where sandbox_id='$1'" | cut -c1-24 }

run() {
  local dir=$1 script=$2
  rm -rf $dir && mkdir -p $dir
  FLOTILLA_DB=$D/demo.db FLOTILLA_SNAPSHOT_DIR=$dir FLOTILLA_SNAPSHOT_SCRIPT=$script $APP > $dir/run.log 2>&1 || true
  grep -E "timed out|unknown" $dir/run.log || true
}

seed() {
  local s1="Write a tiny Python script that prints the first ten prime numbers, run it, and show me the code."
  local s2="What's installed in this VM? Check the node, python and git versions in one command and list them."
  local s3=$'Write a four-line poem about a small boat crossing a bay at dusk.\nRules:\n- four lines, no title\n- mention the tide once\n- end on the harbour lights\n- no rhymes on "sea"\n- keep each line under ten words\n- plain words, nothing fancy\n- save it as poem.txt\n- then print the file'
  local s4="In one sentence, what kind of machine is this? Check /etc/os-release and uname -a."
  # The last one finishes while New thread is open, so it arrives unread.
  run $D/seed "wait:3500;newthread:$s1;wait:6000;until:idle,600;newthread:$s2;wait:6000;until:idle,600;newthread:$s3;wait:6000;until:idle,600;newthread:$s4;wait:4000;step:new-thread;wait:90000;shot:seeded"
}

take() {
  case $1 in
    # 1. The window: groups, a row, resizing and hiding the sidebar, New thread.
    01) run $D/ch01 "wait:3500;rec:start:15;caption:The sidebar;glide:140,63,800;wait:2600;glide:140,177,700;wait:3600;glide:140,692,700;wait:2400;glide:140,725,500;wait:2100;caption:A row;glide:120,100,700;wait:7300;caption:Make room;glide:279,420,600;drag:279,420,390,420,900;wait:700;drag:390,420,280,420,700;wait:400;key:ctrl-cmd-s;wait:1600;key:ctrl-cmd-s;wait:1300;caption:A new thread;glide:730,360,700;wait:4000;rec:stop" ;;
    # 2. A first thread: the model picker, No project, a prompt, the VM starting, the title, time left.
    02) run $D/ch02 "wait:3500;rec:start:15;caption:Any agent, any model;glide:437,426,700;click:437,426;wait:1800;scroll:540,250,-400;wait:2500;key:escape;wait:3400;caption:A blank VM;glide:445,465,600;wait:1400;glide:730,360,600;type:Write a Python script that draws a small sailboat in ASCII art, save it as boat.py, and run it.;wait:3000;glide:1065,426,500;click:1065,426;wait:800;caption:It opens at once;wait:7600;caption:A title from a model;wait:3400;wait:5000;rec:pause;until:idle,600;rec:resume;wait:800;caption:Time left;glide:330,53,700;wait:2600;glide:330,746,700;wait:2600;wait:1200;shot:end;rec:stop" ;;
    # 3. Steer and stop, on chapter 2's thread.
    03) run $D/ch03 "wait:3500;open:Draw ASCII sailboat;wait:1500;rec:start:15;step:send:Now write and run a script that counts from 1 to 10, sleeping two seconds between numbers.;wait:4000;caption:Steer;glide:730,697,600;wait:6500;step:send:Actually, stop at 5 and print done.;wait:800;caption:It changes course;wait:2600;rec:pause;until:idle,300;rec:resume;wait:1500;step:send:Now list every file under /usr/share, one per line, with a short pause between each.;wait:5000;shot:running;caption:Stop;glide:1066,747,600;click:1066,747;wait:4200;wait:1500;shot:stopped;rec:stop" ;;
    # 4. Many at once: a Codex thread (GPT-6 Luna: the ChatGPT subscription covers it; GPT-5.4 Mini
    # needs an OpenAI key) beside a Haiku follow-up, the Working group, the finished toasts
    # (live: they hide after 5 s), the hover card, ⌘1–3.
    04)
      sqlite3 $D/demo.db "update kv set value='codex' where key='default_provider'; update kv set value='gpt-6-luna' where key='default_model';"
      run $D/ch04 "wait:3500;rec:start:15;caption:A second agent;glide:437,426,600;wait:1200;glide:730,360,500;type:In one short paragraph, what is exponential backoff with jitter?;wait:500;glide:1065,426,400;click:1065,426;wait:1300;open:Python script printing;wait:600;step:send:Now also print how many primes there are below one hundred.;wait:900;caption:Working;glide:140,692,500;click:140,692;wait:3300;shot:working;step:new-thread;wait:5000;caption:Finished;glide:1060,90,700;wait:6000;shot:finished;wait:8000;caption:The hover card;glide:120,100,700;wait:4600;shot:card;caption:Jump around;glide:730,400,400;key:cmd-1;wait:800;key:cmd-2;wait:800;key:cmd-3;wait:800;shot:end;rec:stop"
      sqlite3 $D/demo.db "update kv set value='claude-code' where key='default_provider'; update kv set value='claude-haiku-5-5' where key='default_model';" ;;
    # 5. Reading and replying: a reply's footer, Cite, a code block's copy, a folded prompt, ↑.
    05) run $D/ch05 "wait:3500;open:Python script printing;wait:1800;rec:start:15;caption:Copy and cite;glide:700,609,600;wait:1300;rclick:600,609;wait:1000;glide:671,690,400;click:671,690;wait:2800;shot:cited;caption:Code blocks;glide:1133,510,600;wait:1500;caption:Recall;open:Dusk Crossing;wait:900;glide:378,298,400;click:378,298;wait:900;glide:730,697,400;click:730,697;key:up;wait:1500;shot:recalled;rec:stop" ;;
    # 6. VMs that sleep: the idle stop in Settings (one short scroll: further down, a scratch DB lists
    # environments of projects it doesn't know), a stopped VM waking on a send, the Keep running
    # cup, Preview's port dialog (the browser itself isn't in the window).
    06) run $D/ch06 "wait:3500;rec:start:15;caption:Idle stop;glide:257,762,600;click:257,762;wait:1000;scroll:590,500,-300;wait:900;glide:410,428,600;wait:2800;key:escape;wait:500;caption:Stopped;open:What machine is this;wait:1500;glide:300,53,500;wait:800;shot:stopped;step:send:Which kernel version is it running? One line.;wait:2800;rec:pause;wait:12000;until:idle,300;rec:resume;wait:1500;shot:woke;caption:Keep running;glide:1108,24,600;click:1108,24;wait:2900;caption:Preview;glide:1030,24,500;click:1030,24;wait:2400;shot:preview;key:escape;wait:2000;rec:stop"
      # Keep running stays on only for the shot.
      $root/target/debug/flotilla --db $D/demo.db keep-running $(sqlite3 $D/demo.db "select id from threads where title like 'What machine is this%'") off > /dev/null ;;
    # 7. Keeping it tidy: rename with Generate, archive and ⌘Z, unarchive by sending, delete.
    07) run $D/ch07 "wait:3500;open:Dusk Crossing;wait:1200;rec:start:15;caption:Rename;glide:400,24,600;dclick:400,24;wait:800;glide:749,139,400;click:749,139;wait:3500;glide:766,187,400;click:766,187;wait:1200;shot:renamed;caption:Archive;glide:1148,24,500;click:1148,24;wait:600;glide:1084,89,300;click:1084,89;wait:1800;shot:archived;key:cmd-z;wait:1800;caption:Unarchive;click:1148,24;wait:600;click:1084,89;wait:1500;step:send:Read the poem back to me, one line.;wait:2000;rec:pause;wait:10000;until:idle,300;rec:resume;wait:1000;shot:back;caption:Delete;open:Checking installed;wait:900;glide:1148,24,500;click:1148,24;wait:600;glide:1086,245,300;click:1086,245;wait:900;glide:765,195,400;click:765,195;wait:2000;shot:deleted;rec:stop" ;;
    # 8. Add a repo. The repo list's background refresh drops a pick made before it lands (an
    # app bug), and suggestions wait for it, so the take pauses before picking. 08 ends at the
    # Add click; 08b opens the project page and rebuilds from scratch for the live stages.
    08) run $D/ch08 "wait:3500;rec:start:15;caption:Projects;glide:257,762,600;click:257,762;wait:900;glide:450,279,600;wait:5300;caption:Add a project;glide:790,330,600;click:790,330;step:search-repos:flotilla-playground;wait:1500;rec:pause;wait:45000;rec:resume;wait:300;glide:432,204,600;click:432,204;wait:2400;rec:pause;wait:12000;rec:resume;wait:300;caption:Setup lives in the repo;glide:420,307,600;wait:5900;glide:827,373,600;click:827,373;wait:1500;rec:stop" ;;
    08b) run $D/ch08b "wait:3500;step:settings;wait:1200;rec:start:15;caption:The first setup;glide:808,374,600;click:808,374;wait:1200;glide:498,268,600;click:498,268;wait:900;glide:762,206,500;click:762,206;rec:pause;until:setup-running,120;rec:resume;wait:7000;rec:pause;until:setup-done,900;rec:resume;wait:800;caption:Ready;wait:5900;rec:stop" ;;
    # 9. A project thread: project and New branch, the start card, services, Preview ▾; then a
    # still of the page (09s: the browser isn't in the window; built locally from main).
    09) run $D/ch09 "wait:3500;step:new-thread;wait:1500;rec:start:15;caption:A branch per thread;glide:445,465,600;click:445,465;wait:700;glide:468,497,400;click:468,497;wait:800;glide:600,465,500;click:600,465;wait:2600;glide:605,520,400;click:605,520;wait:1300;glide:730,360,600;type:In the Retry visualizer, show how long each panel's run takes from the first try to the last, next to its busiest-moment stat. Keep the change small, add a changelog line, and open a draft pull request.;wait:700;glide:1065,426,500;click:1065,426;wait:400;caption:Named by a model;wait:2700;caption:From a copy;wait:7500;rec:pause;wait:45000;rec:resume;wait:300;caption:Services;glide:1030,24,600;click:1030,24;wait:5200;key:escape;wait:600;rec:stop" ;;
    09s) still ch09s $D/still-visualizer.png "The visualizer" 4.2 ;;
    # 10. The pull request. 10a: the clean PR bar. Then push the teammate's conflicting commit to
    # main (same stat line and the changelog's first line). 10b: Conflicts and 1 behind,
    # Resolve and send, green again, Ready, press and hold to merge, setup for the new main.
    10a) run $D/ch10a "wait:3500;open:Show retry panel;wait:2500;until:pr,180;wait:1500;rec:start:15;caption:The PR bar;glide:420,654,700;wait:1800;glide:890,654,600;wait:1500;glide:950,654,400;wait:1200;glide:1098,654,500;wait:1600;rec:stop" ;;
    10b) run $D/ch10b "wait:3500;open:Show retry panel;wait:2000;until:conflicts,300;wait:65000;rec:start:15;caption:Main moved on;glide:528,53,700;wait:2500;glide:606,53,400;wait:1500;glide:857,654,700;wait:2800;caption:Resolve;glide:980,654,600;click:980,654;wait:3200;glide:1133,747,700;click:1133,747;wait:1500;rec:pause;wait:8000;until:idle,1200;until:mergeable,600;wait:25000;until:checks-pass,1200;rec:resume;wait:1000;caption:Green again;wait:3600;caption:Hold to merge;glide:1098,654,600;click:1098,654;wait:4500;glide:1090,654,300;press:1090,654;wait:1500;release:1090,654;wait:600;rec:pause;until:merged,240;rec:resume;wait:1200;caption:Setup follows main;step:open-project:flotilla-playground;rec:pause;until:setup-running,120;rec:resume;wait:4500;rec:pause;until:setup-done,600;rec:resume;wait:2500;rec:stop" ;;
    # 11. When things break. Before: a second project thread whose task ends in a draft PR
    # ("Add a Reset button …"), then a teammate's typo pushed to its branch so a test fails.
    # 11a: the red "1 failing" and Fix. 11b: a commit that calls a missing npm script in
    # .agents/setup is pushed 12 s in (see `breaksetup`); the run fails, the toast's Fix in a
    # new thread, the thread starts anyway (its agent goes on to open the fix PR chapter 12 uses).
    # Its first caption is moved to 2.5 s before mixing, as the run starts.
    11a) run $D/ch11a "wait:3500;open:Add Reset button;wait:2000;until:checks-fail,300;wait:1500;rec:start:15;caption:A failing check;glide:838,654,600;wait:1200;glide:996,654,500;click:996,654;wait:2600;rec:stop" ;;
    11b) (/bin/sleep 12; breaksetup) & run $D/ch11b "wait:3500;step:open-project:flotilla-playground;wait:1500;rec:start:15;wait:1200;rec:pause;until:setup-running,240;rec:resume;wait:2500;rec:pause;until:setup-failed,600;rec:resume;wait:1500;caption:Setup broke on main;wait:6600;caption:Fix in a new thread;glide:1054,111,700;step:fix-setup:flotilla-playground;wait:5900;step:new-thread-submit;wait:1500;caption:It starts anyway;wait:2500;rec:pause;wait:30000;rec:resume;wait:4200;rec:stop"; wait ;;
    # 12. Start from a branch or PR: the picker (open PRs, branches, "another thread is on it"),
    # pick #12, then a thread's right-click → New thread on this branch. Before: chapter 11's
    # fix PR open. The picker is closed before the right-click, or the click lands in it.
    12) run $D/ch12 "wait:3500;step:new-thread;wait:1200;step:project:flotilla-playground;wait:1500;rec:start:15;caption:Any branch or PR;glide:600,465,600;click:600,465;wait:1800;glide:690,547,500;wait:900;glide:690,574,400;wait:900;glide:650,601,400;wait:900;step:branch:#12;wait:900;key:escape;wait:700;caption:On this branch;glide:140,360,500;rclick:140,360;wait:300;glide:253,553,400;click:253,553;wait:3000;rec:stop" ;;
    # 13. Hooks and services. 13s: the Four files card. 13a: a wake (thread A's VM stopped off
    # camera, by its idle stop or the CLI) shows ".agents/resume running…" while it waits. 13b:
    # ⋯ → Archive on thread B runs .agents/archive (removing the marker setup left), then the VM
    # stops. 13c: Preview ▾ → web's Output (its request log) and Restart. 13a's caption is moved
    # to 0.4 s before mixing.
    13s) still ch13s $D/card-four-files.png "Four files" 8.6 ;;
    13a) run $D/ch13a "wait:3500;open:Show retry panel;wait:2500;rec:start:15;glide:730,697,600;wait:400;step:send:Which branch is this VM on? One line.;wait:300;caption:Resume;wait:6000;rec:pause;until:idle,300;rec:resume;wait:2200;rec:stop" ;;
    13b) run $D/ch13b "wait:3500;open:Add Reset button;wait:2500;rec:start:15;caption:Archive cleans up;glide:1148,24,600;click:1148,24;wait:800;glide:1013,89,400;click:1013,89;wait:9500;rec:stop" ;;
    13c) run $D/ch13c "wait:3500;open:Fix setup missing;wait:5000;rec:start:15;caption:Services, up close;glide:1030,24,600;click:1030,24;wait:1200;glide:830,96,500;click:830,96;wait:2600;glide:866,266,500;click:866,266;wait:2000;key:escape;wait:400;glide:1030,24,500;click:1030,24;wait:1500;key:escape;wait:500;rec:stop" ;;
    # 7b (file 17). Find your threads: the filter's checkboxes: the project only, No project only, All.
    17) run $D/ch17 "wait:3500;step:new-thread;wait:1500;rec:start:15;caption:Find your threads;glide:68,56,600;click:68,56;wait:900;glide:32,154,500;click:32,154;wait:1700;click:32,154;wait:400;glide:32,132,300;click:32,132;wait:1700;glide:32,97,400;click:32,97;wait:1300;key:escape;wait:900;rec:stop" ;;
    # 13b (file 16). See it change: main's page (16s), a Haiku thread retitling the two panels
    # (16a), the page on its branch (16t). Both stills are local builds of the playground (main,
    # then the thread's branch) served on 127.0.0.1 and shot at 1180×780: the app's recorder
    # can't capture the browser.
    16s) still ch16s $D/still-before.png "See it change" 4.8 ;;
    16a) run $D/ch16a "wait:3500;step:new-thread;wait:1500;step:project:flotilla-playground;wait:1200;rec:start:15;caption:Ask for a change;glide:730,360,600;type:In the Retry visualizer, retitle the two panels: Without jitter becomes Without jitter, everyone retries together, and With jitter becomes With jitter, the callers spread out. Keep it small, add a changelog line, and open a draft pull request.;wait:900;glide:1065,426,500;click:1065,426;wait:2500;rec:pause;wait:20000;until:idle,600;until:pr,240;wait:4000;rec:resume;wait:600;caption:Rebuilt and pushed;glide:600,560,700;wait:2200;glide:420,654,700;wait:3400;rec:stop" ;;
    16t) still ch16t $D/still-after.png "The change" 4.4 ;;
    # 13c (file 18). A fleet at work. Before it, off camera: three project threads with small
    # visualizer tasks (two Haiku, one Codex on GPT-6 Luna, started in separate runs since the
    # model comes from the kv defaults, and a few seconds apart: a second New thread sent while the
    # first is still starting was dropped). They finish in a minute or two, so the take shows
    # their fresh draft PRs: hover cards for three, then the Codex one opened.
    18) run $D/ch18 "wait:3500;step:new-thread;wait:1500;rec:start:15;caption:A fleet at work;glide:140,140,700;wait:2700;glide:140,240,600;wait:2500;glide:140,340,600;wait:2400;glide:140,550,600;wait:1500;caption:Each on its own branch;glide:140,130,600;wait:1600;click:140,130;wait:4600;rec:stop" ;;
    # 14. Settings: the top, then one scroll; it stops above the environment list (a scratch DB
    # lists environments of projects it doesn't know, which are implementation details).
    14) run $D/ch14 "wait:3500;step:new-thread;wait:1500;rec:start:15;caption:Settings;glide:257,762,600;click:257,762;wait:3800;glide:420,260,500;scroll:590,500,-300;wait:2200;glide:420,330,600;wait:2400;glide:420,560,600;wait:1800;key:escape;wait:900;rec:stop" ;;
    # 15. Close: the sidebar, then the sails.
    15) run $D/ch15 "wait:3500;step:new-thread;wait:1500;move:900,700;rec:start:15;caption:That's Flotilla;glide:140,300,1600;wait:1400;glide:730,245,1500;wait:2600;rec:stop" ;;
    # 0. Cold open: 00a starts two quick No-project threads off camera and films the Working
    # group; 00b is 10b's hold, 26.4–29.9 s, cut from its frames (`clip`).
    00a) run $D/ch00a "wait:3500;step:new-thread;wait:1500;step:project:none;wait:800;newthread:Write a Python script that counts down from 40, sleeping a second between numbers, run it, and tell me when it's done.;wait:15000;step:new-thread;wait:1500;step:project:none;wait:800;newthread:Write a short shell script that lists the ten largest files under /usr/lib with their sizes, run it, and summarize what you find.;wait:9000;step:new-thread;wait:1200;rec:start:15;caption:Flotilla;glide:55,692,600;click:55,692;wait:1500;glide:140,600,700;wait:1500;glide:140,520,500;wait:1800;wait:1200;rec:stop;wait:20000" ;;
    00b) clip ch10b ch00b 26.4 29.9 "Hold to merge, at a glance" ;;
    # 10b (file 19). The PR, up close: the checks (the bar's toggle, opened and closed again) and
    # a project thread's menu (Open PR, Copy branch name, Link a PR), hovered, not clicked.
    19) run $D/ch19 "wait:3500;open:Retitle Retry;wait:2000;rec:start:15;caption:Checks up close;glide:890,654,600;click:890,654;wait:1200;glide:1120,596,500;wait:1100;glide:1120,618,400;wait:1000;glide:890,654,400;click:890,654;wait:500;caption:Its menu;glide:140,441,600;rclick:140,441;wait:900;glide:230,578,500;wait:1200;glide:220,606,400;wait:1200;glide:200,662,500;wait:1500;key:escape;wait:600;rec:stop" ;;
    # 11b (file 20). Setup, your way: the project page's history (with chapter 11's failure), its
    # scripts, Run setup again. The run is waited out before quitting (a quit mid-run fails it).
    20) run $D/ch20 "wait:3500;step:open-project:flotilla-playground;wait:2500;rec:start:15;caption:Setup history;glide:400,470,700;wait:1700;glide:400,530,500;wait:1500;glide:400,590,500;wait:2000;caption:Your scripts;glide:344,374,600;wait:1900;glide:348,266,600;click:348,266;wait:3600;rec:stop;until:setup-running,60;until:setup-done,600;wait:1500" ;;
    # 11c (file 22). The setup log: View log on the project page.
    22) run $D/ch22 "wait:3500;step:open-project:flotilla-playground;wait:2500;rec:start:15;caption:The setup log;glide:624,266,600;click:624,266;wait:1500;glide:520,380,700;wait:1600;glide:560,450,600;wait:1600;rec:stop" ;;
    # 13d (file 21). Day and Night: Settings → Appearance → Dark, a look around, then back to
    # System off camera.
    21) run $D/ch21 "wait:3500;open:Retitle Retry;wait:2000;rec:start:15;caption:Day and Night;glide:257,762,600;click:257,762;wait:1000;glide:464,558,500;click:464,558;wait:1200;key:escape;wait:400;glide:730,300,800;wait:1500;glide:140,300,800;wait:1500;rec:stop;click:257,762;wait:1000;click:358,558;wait:800;key:escape;wait:800" ;;
    *) print -u2 "no take $1"; exit 1 ;;
  esac
}

# Push the commit that breaks .agents/setup on main, from a scratch clone ($PG) that has it
# committed (one line: `npm run generate` before `npm run build`).
breaksetup() {
  cd ${PG:-/tmp/pg-video} && git -c credential.helper= -c credential.helper='!gh auth git-credential' push -q origin main
}

# A still as a take of its own: one frame held for `secs` under one caption (a card, or a page
# the app's recorder can't capture).
still() {
  local name=$1 png=$2 caption=$3 secs=$4
  rm -rf "${D:?}/${name:?}" && mkdir -p "$D/$name/frames"
  cp "$png" "$D/$name/frames/f00001.png"
  printf "file 'frames/f00001.png'\nduration %s\nfile 'frames/f00001.png'\n" $secs > "$D/$name/frames.txt"
  printf "0.0000\t%s\n%s\t\n" "$caption" $secs > "$D/$name/captions.tsv"
}

# A take cut from another take's frames: `clip <from> <to> <start s> <end s> <caption>`.
clip() {
  local from=$1 to=$2 start=$3 end=$4 caption=$5
  rm -rf "${D:?}/${to:?}" && mkdir -p "$D/$to/frames"
  python3 - "$D/$from" "$D/$to" $start $end "$caption" <<'PY'
import shutil, sys
src, dst, start, end, caption = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
entries, cur = [], None
for l in open(f"{src}/frames.txt").read().split("\n"):
    if l.startswith("file"):
        cur = l.split("'")[1]
    elif l.startswith("duration") and cur:
        entries.append((cur, float(l.split()[1]))); cur = None
t, out = 0.0, []
for f, d in entries:
    if t + d > start and t < end:
        out.append((f, d))
    t += d
lst, total = [], 0.0
for i, (f, d) in enumerate(out):
    shutil.copy(f"{src}/{f}", f"{dst}/frames/f{i + 1:05}.png")
    lst.append(f"file 'frames/f{i + 1:05}.png'\nduration {d:.4f}\n")
    total += d
lst.append(f"file 'frames/f{len(out):05}.png'\n")
open(f"{dst}/frames.txt", "w").write("".join(lst))
open(f"{dst}/captions.tsv", "w").write(f"0.0000\t{caption}\n{total:.4f}\t\n")
PY
}

# The whole video, chapters in order with short fades, into ~/Movies/Flotilla/flotilla-demo.mp4.
final() {
  local inputs=() filters="" joined="" i=0
  for c in "$@"; do
    local len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $D/$c.mp4)
    local out=$(python3 -c "print(max(0.0, $len - 0.3))")
    inputs+=(-i $D/$c.mp4)
    # Braces: zsh reads `$i:a` as a path modifier.
    filters+="[${i}:v]fade=t=in:st=0:d=0.2,fade=t=out:st=${out}:d=0.3,setpts=PTS-STARTPTS[v${i}];"
    filters+="[${i}:a]afade=t=in:st=0:d=0.1,afade=t=out:st=${out}:d=0.3,asetpts=PTS-STARTPTS[a${i}];"
    joined+="[v${i}][a${i}]"
    i=$((i + 1))
  done
  mkdir -p ~/Movies/Flotilla
  ffmpeg -hide_banner -loglevel error -y $inputs -filter_complex "${filters}${joined}concat=n=$#:v=1:a=1[v][a]" -map "[v]" -map "[a]" \
    -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart ~/Movies/Flotilla/flotilla-demo.mp4
  ffprobe -v error -show_entries format=duration -of csv=p=0 ~/Movies/Flotilla/flotilla-demo.mp4
}

# Join narrated takes into one chapter: `join <chapter> <take>…` (each take already mixed).
join() {
  local out=$1; shift
  local inputs=() filters="" i=0
  for t in "$@"; do inputs+=(-i $D/$t.mp4); filters+="[${i}:v][${i}:a]"; i=$((i + 1)); done
  ffmpeg -hide_banner -loglevel error -y $inputs -filter_complex "${filters}concat=n=$#:v=1:a=1[v][a]" -map "[v]" -map "[a]" \
    -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart $D/$out.mp4.tmp.mp4
  mv $D/$out.mp4.tmp.mp4 $D/$out.mp4
}

# Hold a take's last frame longer: `hold <take> <seconds>` (frames.txt and captions.tsv), for
# when its narration runs past its end. The Marin one-take cut needs `hold ch01 2.5`,
# `hold ch16s 1.3` and `hold ch00b 0.8`.
hold() {
  python3 - "$D/$1" "$2" <<'PY'
import sys
d, extra = sys.argv[1], float(sys.argv[2])
lines = open(f"{d}/frames.txt").read().splitlines()
i = max(n for n, l in enumerate(lines) if l.startswith("duration "))
lines[i] = f"duration {float(lines[i].split()[1]) + extra:.4f}"
open(f"{d}/frames.txt", "w").write("\n".join(lines) + "\n")
caps = open(f"{d}/captions.tsv").read().splitlines()
t, rest = caps[-1].split("\t", 1)
caps[-1] = f"{float(t) + extra:.4f}\t{rest}"
open(f"{d}/captions.tsv", "w").write("\n".join(caps) + "\n")
PY
}

# The cold open, 00 = 00a + 00b: 00b comes from much later footage (its sidebar doesn't match
# 00a's), so it's a close-up on the conversation and its PR bar, and the cut between them
# dips through the icon's plum, reading as time passing.
coldopen() {
  local plum=0x311E36 a=$D/ch00a.mp4 b=$D/ch00b.mp4
  local la=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $a)
  local fa=$(python3 -c "print(max(0.0, $la - 0.3))")
  ffmpeg -hide_banner -loglevel error -y -i $a -i $b -filter_complex "\
[0:v]fade=t=out:st=${fa}:d=0.3:color=${plum},setpts=PTS-STARTPTS[va];\
[1:v]split[b1][b2];[b1]crop=1180:780:0:0,crop=880:582:300:198,scale=1180:780:flags=lanczos[zoom];\
[b2]crop=1180:64:0:780[strip];[zoom][strip]vstack,fade=t=in:st=0:d=0.3:color=${plum},setpts=PTS-STARTPTS[vb];\
[0:a]afade=t=out:st=${fa}:d=0.3,asetpts=PTS-STARTPTS[aa];[1:a]asetpts=PTS-STARTPTS[ab];\
[va][aa][vb][ab]concat=n=2:v=1:a=1[v][au]" -map "[v]" -map "[au]" \
    -c:v libx264 -crf 20 -preset medium -c:a aac -b:a 160k -movflags +faststart $D/ch00.mp4
}

case ${1:-} in
  seed) seed ;;
  take) take $2 ;;
  voice) cd $root && $narrate say $root/script/demo/full-demo.tsv $V ;;
  # One take per chunk of ~12 lines (steadier voice): `take <chunk.tsv> <dir>`, then merge narration.tsv files.
  take) cd $root && $narrate take $2 $3 marin ;;
  hold) hold $2 $3 ;;
  coldopen) coldopen ;;
  mix)
    $narrate mix $D/ch$2/captions.tsv $V/narration.tsv $D/ch$2/narration.wav
    zsh $root/script/demo/assemble.sh $D/ch$2 $D/ch$2.mp4 $D/ch$2/narration.wav ;;
  # Chapters from several takes (each mixed first): 08 = 08 + 08b, 09 = 09 + 09s,
  # 10 = 10a + 10b, 11 = 11a + 11b, 13 = 13s + 13a + 13b + 13c.
  join) shift; join "$@" ;;
  # The final cut: 00, 01–07, 17 (Find your threads), 08–10, 19 (The PR, up close), 11, 20
  # (Setup, your way), 22 (The setup log), 12, 13, 16 (See it change), 18 (A fleet at work),
  # 21 (Day and Night), 14, 15. Chapter 00 = coldopen (00a + a close-up of 00b) and 16 = 16s + 16a + 16t are joined first.
  final) final ch00 ch01 ch02 ch03 ch04 ch05 ch06 ch07 ch17 ch08 ch09 ch10 ch19 ch11 ch20 ch22 ch12 ch13 ch16 ch18 ch21 ch14 ch15 ;;
  *) print -u2 "usage: $0 seed | take <NN> | voice | mix <NN> | join <chapter> <take>… | coldopen | final"; exit 1 ;;
esac
