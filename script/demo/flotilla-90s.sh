#!/bin/zsh
# The Flotilla demo video (about two minutes), for someone who has never heard of it: the Mac app and the iPhone
# app on one server, narrated by Marin (OpenAI gpt-audio via OpenRouter, script/demo/flotilla-90s.tsv),
# into $OUT (default ~/Movies/Flotilla/flotilla-90s.mp4; draft 3 went to flotilla-demo.mp4).
#
#   script/demo/flotilla-90s.sh server      a scratch server (Flotilla Dev) in $D/srv; leave it running
#   script/demo/flotilla-90s.sh setup       pair the Mac app with it, add the playground project
#   script/demo/flotilla-90s.sh seed        off camera: R (Opus 5.5 gets reviews from Codex on GPT-6.1 Sol
#                                           and Claude Code on Fable 5.1), C (OpenCode: a docs pull request,
#                                           merged on the phone), H (Haiku: the one that sleeps and wakes)
#   script/demo/flotilla-90s.sh live        on camera: New thread on Codex (B), its start, its pull request
#   script/demo/flotilla-90s.sh mac         the four at work, B's merge, R's reviews, H waking, a schedule
#   script/demo/flotilla-90s.sh ios         build the iPhone app and its video tests for the simulator in $SIM
#   script/demo/flotilla-90s.sh pairphone   pair the phone with the server, off camera
#   script/demo/flotilla-90s.sh phone       Hold to merge by touch on $MERGE_TITLE's PR
#   script/demo/flotilla-90s.sh ask         the Mac and the iPhone side by side: the phone answers an agent's question
#   script/demo/flotilla-90s.sh sync        the Mac and the iPhone side by side, recorded at once: a reply
#                                           streaming into R on both
#   script/demo/flotilla-90s.sh page        the preview's page for the Mac's browser insert (no address on screen)
#   script/demo/flotilla-90s.sh phonepreview  the iPhone's globe, services sheet and in-app browser (its bar blurred)
#   script/demo/flotilla-90s.sh mcp         Claude Code starts a thread through `flotilla mcp` while the Mac records
#   script/demo/flotilla-90s.sh art         the terminal (flotilla-90s-terminal.txt), the phone frame, the end card
#   script/demo/flotilla-90s.sh voice       voice the whole script, $TAKES times (default 4, about $0.08 each),
#                                           and score the takes side by side (narrate.py check)
#   script/demo/flotilla-90s.sh pick <n>    use take n
#   script/demo/flotilla-90s.sh build       cut, narrate and join
#   script/demo/flotilla-90s.sh publish     the site's and README's copies (flotilla-demo.mp4), poster and GIF
#   script/demo/flotilla-90s.sh scan        OCR every frame of the draft against the forbidden list
#
# Recorded as Flotilla Dev (FLOTILLA_DEV=1: its own database, Keychain item and VM labels), so the
# release app and its server never see these threads. Give the server a short-lived boat key of
# its own in BOAT_API_KEY (`boat api-key create … --ttl 4h`), and revoke it after. Delete the VMs
# after: `boat list --all`, then `boat delete <id> --yes` for each labelled flotilla=dev with this
# server's flotilla.server.
#
# The server answers at http://box.localhost:$PORT: *.localhost is this Mac, and the phone's chip
# shows a server's first label, so it reads "box". iOS only lets the app reach that over plain
# HTTP with an exception, which `ios` adds to project.yml for its build and takes out again.
#
# Nothing on screen may name a real person, machine, account, tailnet, path or id. The playground
# repo lives under a real GitHub account, so: no hover cards over thread rows (they name the
# project's owner), no Settings, and the prompts ask for replies without links. `scan` checks.
#
# `open:` finds a thread by its title, which a model writes: after a new recording, check them
# (`sqlite3 $D/srv/flotilla.db 'select provider, model, title from threads'`).
#
# Build the Mac app first:
#   cargo build --release -p flotilla-app --features snapshot -p flotilla-cli --target-dir target/demo
set -eu
root=${0:A:h:h:h}
D=${FLOTILLA_90S_DIR:-/tmp/flotilla-90s}
PORT=${PORT:-7812}
SIM=${SIM:-$(cat $D/sim 2>/dev/null || true)}
APP=$root/target/demo/release/flotilla-app
CLI=$root/target/demo/release/flotilla
URL=http://box.localhost:$PORT
narrate=(uv run --quiet --python 3.13 --with mlx-audio==0.4.2 --with soundfile python $root/script/demo/narrate.py)
db() { sqlite3 $D/srv/flotilla.db "$1" }
# A thread's id by its title.
titled() { db "select id from threads where title like '$1%' order by created_at desc limit 1" }
link() { FLOTILLA_DEV=1 $CLI pair --data-dir $D/srv --ttl ${1:-10m} | grep -o 'flotilla[-a-z]*://pair[^ ]*' }

# One scripted run of the Mac app (Flotilla Dev, on the server device.json names) into $D/runs/<take>.
mac_run() {
  local take=$1 script=$2
  rm -rf "${D:?}/runs/${take:?}" && mkdir -p $D/runs/$take
  FLOTILLA_DEV=1 FLOTILLA_DEMO=1 FLOTILLA_DB=$D/mac/flotilla.db FLOTILLA_SNAPSHOT_DIR=$D/runs/$take FLOTILLA_SNAPSHOT_SCRIPT=$script \
    $APP > $D/runs/$take/run.log 2>&1 || true
  grep -E "timed out|unknown" $D/runs/$take/run.log || true
  # The recorder's frames at 30 fps, for the build.
  if [[ -f $D/runs/$take/frames.txt ]]; then
    (cd $D/runs/$take && ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i frames.txt -vf "fps=30,format=yuv420p" -c:v libx264 -crf 14 -preset veryfast clip.mp4)
  fi
}

# One iPhone scene: record the simulator while a VideoUITests test runs, then put its frames
# in order (simframes) at 30 fps, as $D/phone/<scene>-fixed.mp4. NAME=value pairs go to the test.
phone_run() {
  local scene=$1 test=$2; shift 2
  local P=$D/phone; mkdir -p $P
  rm -f $P/$scene.{mp4,touches,t0,log,go,ready}
  local envs=(TEST_RUNNER_FLOTILLA_TOUCHES=$P/$scene.touches TEST_RUNNER_FLOTILLA_GO=$P/$scene.go TEST_RUNNER_FLOTILLA_READY=$P/$scene.ready)
  for kv in "$@"; do envs+=(TEST_RUNNER_$kv); done
  xcrun simctl io $SIM recordVideo --codec=h264 --force $P/$scene.mp4 > $P/$scene.rec.log 2>&1 &
  local rec=$!
  for i in {1..100}; do grep -q "Recording started" $P/$scene.rec.log 2>/dev/null && break; /bin/sleep 0.1; done
  python3 -c "import time; print(f'{time.time():.3f}')" > $P/$scene.t0
  if [[ -n ${WAIT_READY:-} ]]; then
    # Go once the test says the screen is ready (it writes $scene.ready); the Mac waits for go too.
    ( for i in {1..900}; do [[ -f $P/$scene.ready ]] && break; /bin/sleep 0.1; done; touch $P/$scene.go ) &
  else
    touch $P/$scene.go
  fi
  (cd $root/ios && env $envs xcodebuild -scheme Flotilla -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath build/dd \
    test-without-building -only-testing:FlotillaUITests/VideoUITests/$test > $P/$scene.log 2>&1) || print -u2 "$scene: the test failed (see $P/$scene.log)"
  /bin/sleep 0.5
  kill -INT $rec; wait $rec 2>/dev/null || true
  rm -rf "${P:?}/${scene:?}-frames"
  $D/simframes $P/$scene.mp4 $P/$scene-frames 0.5
  (cd $P/$scene-frames && ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i frames.txt -vf "fps=30,scale=604:1312,format=yuv420p" -c:v libx264 -crf 16 -preset veryfast $P/$scene-fixed.mp4)
}

# The prompts. Each asks for a reply without links: the repo's owner is in every GitHub URL.
QUIET="When you're done, reply in two or three short lines, with no links or URLs."
R="Write a slugify(title) function for URL slugs in ~/slug.py, with a few tests. Then get second opinions from Codex (gpt-6.1-sol) and from Claude Code on Fable (claude-fable-5-1): each should review it and list any bugs. Fix what they find, and tell me in three lines who caught what."
C="Add a Usage section to README.md with a short example of retry with a custom maxMs. Add a line at the end of the Unreleased list in CHANGELOG.md, then open a pull request (not a draft). $QUIET"
H="Write a small Python script in ~/words.py that counts the words in a text file and prints the ten most common, with a test. Run the test. $QUIET"
B="Add an onRetry(attempt, error, waitMs) callback to retry, with a test and a changelog line. Then open a pull request (not a draft). $QUIET"

case ${1:-} in
  server)
    mkdir -p $D/srv
    : ${BOAT_API_KEY:?give the server its own short-lived boat key}
    FLOTILLA_DEV=1 exec $CLI serve --data-dir $D/srv --port $PORT --url $URL ;;
  setup)
    mkdir -p $D/mac
    link > $D/link-mac
    FLOTILLA_OFFLINE=1 mac_run pair "wait:3000;step:connect-file:$D/link-mac;wait:1500;step:connect-submit;until:online,40;wait:3000"
    python3 -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d['appearance']='light'; json.dump(d,open(p,'w'),indent=2)" $D/mac/device.json
    # The repo list refreshes once after it opens, dropping a pick made before: search, wait, pick.
    mac_run addproj "wait:3000;step:add-project;until:repos,90;step:search-repos:flotilla-playground;wait:30000;step:pick-repo:flotilla-playground;wait:10000;step:add-project-submit;wait:5000;until:setup-running,60;until:setup-done,900" ;;
  seed)
    # R first (it takes longest), then C and H on the playground. Each waits for its VM to wake.
    mac_run seed "wait:3500;step:new-thread;wait:1500;step:project:none;wait:800;step:agent:claude-code/opus-5-5/high;wait:600;newthread:$R;wait:15000;until:woken,900;step:new-thread;wait:1500;step:project:flotilla-playground;wait:800;step:agent:opencode/${OPENCODE_MODEL:-gpt-6-luna};wait:600;newthread:$C;wait:15000;until:woken,900;step:new-thread;wait:1500;step:project:none;wait:800;step:agent:claude-code/claude-haiku-5-5/low;wait:600;newthread:$H;wait:15000;until:woken,900;wait:5000" ;;
  live)
    # B from New thread, on camera: type the task, pick Codex and its model, Start; then its start
    # and its work, until the pull request is up. Coordinates are read off `shot`s of this window.
    mac_run live "${LIVE_SCRIPT:?set LIVE_SCRIPT from a calibration shot}" ;;
  mac)
    mac_run ${2:?take} "${MAC_SCRIPT:?}" ;;
  ios)
    [[ -n $SIM ]] || { print -u2 "make a simulator: SIM=\$(xcrun simctl create 'Flotilla video' 'iPhone 17') and write it to $D/sim"; exit 1 }
    xcrun simctl boot $SIM 2>/dev/null || true
    swiftc -O -o $D/simframes $root/script/demo/simframes.swift
    cd $root/ios
    python3 - <<'PY'
p = "project.yml"
s = open(p).read()
old = "        NSAppTransportSecurity:\n          NSAllowsLocalNetworking: true\n"
open(p, "w").write(s.replace(old, old + "          NSExceptionDomains:\n            localhost:\n              NSIncludesSubdomains: true\n              NSExceptionAllowsInsecureHTTPLoads: true\n"))
PY
    xcodegen > /dev/null
    git checkout project.yml
    xcodebuild -scheme Flotilla -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath build/dd build-for-testing > $D/ios-build.log 2>&1
    # Answer the notifications question once, off camera.
    xcodebuild -scheme Flotilla -destination "platform=iOS Simulator,id=$SIM" -derivedDataPath build/dd test-without-building -only-testing:FlotillaUITests/VideoUITests/testV0Allow > /dev/null 2>&1 || true ;;
  pairphone)
    # Pairing isn't in the video: pair the phone once, off camera.
    phone_run pair testV1Pair "FLOTILLA_LINK=$(link 15m)" ;;
  phone)
    # Hold to merge, by touch, on the PR of the thread titled $MERGE_TITLE.
    phone_run merge testV5HoldToMerge FLOTILLA_MERGE_THREAD=$(titled "${MERGE_TITLE:?the title of the thread whose PR merges}") ;;
  phonepreview)
    # Preview on the phone: the globe, the services sheet, Open, the page in the in-app browser.
    phone_run phonepreview testV10Preview FLOTILLA_PREVIEW_THREAD=$(titled "${PREVIEW_TITLE:?the title of the thread whose dev server to open}")
    # The in-app browser's bar shows the VM's address: blur it from the Open tap (the second
    # touch after the scene's start) to the end. Check with `scan`.
    P=$D/phone; mv $P/phonepreview-fixed.mp4 $P/phonepreview-raw.mp4
    from=$(python3 -c "import sys
t0 = float(open(sys.argv[1]).read())
rows = [list(map(float, l.split())) for l in open(sys.argv[2])]
go = next(i for i, r in enumerate(rows) if r[3] < 0)
print(f'{rows[go + 2][0] - t0 + 0.2:.2f}')" $P/phonepreview.t0 $P/phonepreview.touches)
    ffmpeg -hide_banner -loglevel error -y -i $P/phonepreview-raw.mp4 -filter_complex "[0:v]split[a][b];[b]crop=430:80:88:88,boxblur=luma_radius=22:luma_power=3:chroma_radius=10:chroma_power=2[bl];[a][bl]overlay=88:88:enable='gte(t,$from)'[v]" -map "[v]" -c:v libx264 -crf 16 -preset veryfast -pix_fmt yuv420p $P/phonepreview-fixed.mp4 ;;
  page)
    # The preview's page as the Mac's browser shows it, for the insert (flotilla-90s-art.py
    # browser): the thread's private preview URL into a file only you can read, headless Chrome
    # with a throwaway profile, then the URL goes. Nothing on screen shows the address.
    t=$(titled "${PREVIEW_TITLE:?}"); mkdir -p $D/browser; umask 077
    FLOTILLA_DEV=1 $CLI --data-dir $D/srv preview $t ${PREVIEW_PORT:-4100} > $D/browser/url
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --user-data-dir=$D/browser/profile --no-first-run --no-default-browser-check \
      --disable-extensions --hide-scrollbars --force-device-scale-factor=2 --window-size=1040,1500 --virtual-time-budget=6000 --screenshot=$D/browser/page.png "$(cat $D/browser/url)" > /dev/null 2>&1
    rm -P $D/browser/url; rm -rf "${D:?}/browser/profile"
    uv run --quiet --with pillow python $root/script/demo/flotilla-90s-art.py browser $D/art $D/browser/page.png
    mkdir -p $D/runs/browser
    ffmpeg -hide_banner -loglevel error -y -framerate 30 -i $D/art/browser/%04d.png -c:v libx264 -crf 14 -pix_fmt yuv420p $D/runs/browser/clip.mp4 ;;
  ask)
    # The Mac and the phone side by side on a thread whose agent asked a question ($ASK_TITLE):
    # each opens it and waits; on go both record while the phone picks the first option and
    # sends it, and the Mac shows it answered and the agent going on.
    a=$(titled "${ASK_TITLE:?the title of the thread with the question}")
    mac_run ask "wait:3500;open:$ASK_TITLE;wait:2500;until:asked,120;move:900,600;until:file:$D/phone/ask.go,300;rec:start:15;caption:ask;wait:${ASK_MS:-16000};rec:stop;wait:500" &
    mac=$!
    WAIT_READY=1 phone_run ask testV9Ask FLOTILLA_ASK_THREAD=$a FLOTILLA_WATCH=${ASK_WATCH:-14}
    wait $mac ;;
  sync)
    # The Mac and the phone side by side on R. Each opens it and waits; on go both record (the
    # Mac notes when, in rec-start.txt, to line the two up) while the Mac sends a follow-up and
    # the reply streams in.
    s=$(titled "${SYNC_TITLE:?the title of R}")
    ask=${SYNC_ASK:-"Without running anything, explain in two short paragraphs how slugify decides where to cut a long title."}
    mac_run sync "wait:3500;open:$SYNC_TITLE;wait:2500;move:900,560;until:file:$D/phone/sync.go,300;rec:start:15;caption:sync;wait:1500;glide:${SYNC_BOX:-730,697},700;wait:300;type:$ask;wait:700;glide:${SYNC_SEND:-1133,747},500;click:${SYNC_SEND:-1133,747};wait:${SYNC_MS:-40000};rec:stop;wait:500" &
    mac=$!
    WAIT_READY=1 phone_run sync testV8Sync FLOTILLA_SYNC_THREAD=$s FLOTILLA_WATCH=$(( ${SYNC_MS:-40000} / 1000 + 4 ))
    wait $mac ;;
  mcp)
    # Claude Code using Flotilla through `flotilla mcp` (the stdio MCP server, as an agent device):
    # it starts a thread and reads its answer, while the Mac records the thread appearing. Its
    # stream (each event with the time it came) is in $D/mcp/stream.jsonl, for the terminal drawn
    # over the Mac's take (flotilla-90s-art.py mcp).
    mkdir -p $D/mcp
    python3 -c "import json,sys; json.dump({'mcpServers': {'flotilla': {'command': sys.argv[1], 'args': ['mcp', '--data-dir', sys.argv[2]], 'env': {'FLOTILLA_DEV': '1'}}}}, open(sys.argv[3], 'w'))" $CLI $D/srv $D/mcp/config.json
    rm -f $D/mcp/go
    mac_run mcp "wait:3500;step:new-thread;wait:1500;move:760,560;until:file:$D/mcp/go,120;rec:start:15;caption:mcp;wait:${MCP_MS:-75000};rec:stop;wait:500;shot:after" &
    mac=$!
    for i in {1..150}; do [[ -f $D/runs/mcp/run.log ]] && grep -q "until:file" $D/runs/mcp/run.log && break; /bin/sleep 0.2; done
    /bin/sleep 1
    python3 -c "import time; print(f'{time.time():.3f}')" > $D/mcp/t0; touch $D/mcp/go
    claude -p "${MCP_ASK:?the request to Claude Code}" --model ${MCP_MODEL:-sonnet} --mcp-config $D/mcp/config.json --strict-mcp-config --allowedTools mcp__flotilla --tools "" --output-format stream-json --verbose \
      | python3 -u -c "import sys,time,json
for line in sys.stdin:
    sys.stdout.write(json.dumps({'t': time.time(), 'e': json.loads(line)}) + '\\n'); sys.stdout.flush()" > $D/mcp/stream.jsonl
    wait $mac
    # Its terminal, drawn from the stream at the Mac recording's times, laid over the Mac's take.
    uv run --quiet --with pillow python $root/script/demo/flotilla-90s-art.py mcp $D/art $D/mcp/stream.jsonl $D/mcp/t0 $D/runs/mcp/rec-start.txt
    mkdir -p $D/runs/mcpcomp
    ffmpeg -hide_banner -loglevel error -y -i $D/runs/mcp/clip.mp4 -framerate 30 -i $D/art/term-mcp/%04d.png -filter_complex "[0:v][1:v]overlay=0:0:shortest=1,format=yuv420p" -c:v libx264 -crf 14 -preset veryfast $D/runs/mcpcomp/clip.mp4 ;;
  art)
    uv run --quiet --with pillow python $root/script/demo/flotilla-90s-art.py $D/art $root/design/icons/AppIcon.png $root/script/demo/flotilla-90s-terminal.txt ;;
  voice)
    cd $root
    for n in $(seq ${TAKES:-4}); do NARRATOR=openrouter $narrate take $root/script/demo/flotilla-90s.tsv $D/takes/$n marin; done
    $narrate check $root/script/demo/flotilla-90s.tsv $D/takes/<->(/) ;;
  pick)
    [[ -d $D/takes/${2:-} ]] || { print -u2 "usage: $0 pick <take number in $D/takes>"; exit 1 }
    rm -rf "${D:?}/voice" && cp -R $D/takes/$2 $D/voice
    sed -i '' "s#$D/takes/$2/#$D/voice/#" $D/voice/narration.tsv ;;
  build)
    out=${OUT:-$HOME/Movies/Flotilla/flotilla-90s.mp4}
    python3 $root/script/demo/flotilla-90s-build.py $D $out $D/voice/narration.tsv
    ffmpeg -hide_banner -loglevel error -y -i $out -ss 2.0 -frames:v 1 $D/poster.png ;;
  publish)
    # The site's and the README's copies, under names Cloudflare hasn't cached: small, and
    # playable before they're all loaded; the poster; the README's GIF (the four at work, then
    # Hold to merge from the press to Merged, wherever the merge segment falls).
    out=${OUT:-$HOME/Movies/Flotilla/flotilla-90s.mp4}
    ffmpeg -hide_banner -loglevel error -y -i $out -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p -c:a aac -ac 1 -b:a 96k -movflags +faststart $root/www/flotilla-demo.mp4
    cp $root/www/flotilla-demo.mp4 $root/docs/flotilla-demo.mp4
    ffmpeg -hide_banner -loglevel error -y -i $out -ss ${POSTER_AT:-1.5} -frames:v 1 $D/poster.png
    uv run --quiet --with pillow python -c "from PIL import Image; import sys; Image.open(sys.argv[1]).convert('RGB').save(sys.argv[2], 'WEBP', quality=82, method=6)" $D/poster.png $root/www/poster-demo.webp
    m=$(awk -F'\t' '$2=="hold" {print $1 - 0.2}' $D/captions.tsv)
    ffmpeg -hide_banner -loglevel error -y -i $out -filter_complex "[0:v]trim=0.6:7.4,setpts=PTS-STARTPTS[a];[0:v]trim=$(( m + 0.9 )):$(( m + 6.05 )),setpts=PTS-STARTPTS[b];[a][b]concat=n=2:v=1:a=0,fps=12,scale=820:-1:flags=lanczos,split[x][y];[x]palettegen=max_colors=128:stats_mode=diff[p];[y][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" $root/docs/images/flotilla.gif
    /bin/ls -l $root/www/flotilla-demo.mp4 $root/docs/flotilla-demo.mp4 $root/www/poster-demo.webp $root/docs/images/flotilla.gif ;;
  scan)
    # Every frame (at 3 fps; SCAN_FPS=30 for all of them) OCR'd, against the forbidden list in
    # $D/forbidden.txt (one case-insensitive extended regex a line: the real names, accounts,
    # tailnet, paths and id shapes; kept out of the repo, since it lists them), plus contact sheets.
    out=${OUT:-$HOME/Movies/Flotilla/flotilla-90s.mp4}
    rm -rf "${D:?}/scan" && mkdir -p $D/scan/frames
    ffmpeg -hide_banner -loglevel error -y -i $out -vf fps=${SCAN_FPS:-3} $D/scan/frames/%05d.png
    swiftc -O -o $D/ocr $root/script/demo/ocr.swift
    $D/ocr $D/scan/frames > $D/scan/ocr.tsv
    grep -i -E -f $D/forbidden.txt $D/scan/ocr.tsv > $D/scan/hits.tsv || true
    ffmpeg -hide_banner -loglevel error -y -i $out -vf "fps=1,scale=295:-1,tile=6x5" $D/scan/sheet-%02d.png
    print "$(wc -l < $D/scan/ocr.tsv) lines of text in $(/bin/ls $D/scan/frames | wc -l) frames; $(wc -l < $D/scan/hits.tsv) hits (see $D/scan/hits.tsv)" ;;
  *) print -u2 "usage: $0 server | setup | seed | live | mac <take> | ios | pairphone | phone | phonepreview | page | ask | mcp | art | voice | pick <n> | build | publish | scan"; exit 1 ;;
esac
