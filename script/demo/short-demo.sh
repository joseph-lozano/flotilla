#!/bin/zsh
# The 60-second Flotilla demo, cut from the long demo's narrated chapters (no new recording),
# narrated by Marin (OpenAI gpt-audio via OpenRouter), into ~/Movies/Flotilla/flotilla-60s.mp4.
#
#   script/demo/short-demo.sh voice    voice script/demo/short-demo.tsv (only changed lines)
#   script/demo/short-demo.sh build    closing card, cuts, narration mix, final encode
#
# Needs the long demo's chapters in $FLOTILLA_DEMO_DIR (default /tmp/flotilla-video: chNN.mp4,
# 1180×844 with a 64 px caption strip at the bottom, cropped off here) and the OpenRouter key
# file narrate.py reads (~/.cache/flotilla-demo/openrouter.key).
set -eu
root=${0:A:h:h:h}
src=${FLOTILLA_DEMO_DIR:-/tmp/flotilla-video}
D=${FLOTILLA_SHORT_DIR:-/tmp/flotilla-short}
out=${FLOTILLA_SHORT_OUT:-$HOME/Movies/Flotilla/flotilla-60s.mp4}
narrate=(uv run --quiet --python 3.13 --with mlx-audio==0.4.2 --with soundfile python $root/script/demo/narrate.py)

# caption (from short-demo.tsv, "-" continues the one before), chapter, in, out, speed (video only).
segments=(
  "fleet    ch00 0.8  6.5  1.0"   # agents working side by side, the hover card
  "new      ch09 3.0  11.0 1.5"   # a new thread in the project, the prompt typed
  "start    ch09 11.5 16.5 1.0"   # name the branch, start a VM on it
  "services ch09 24.5 28.5 1.0"   # Preview: web and info ready
  "-        ch09 32.5 35.0 1.0"   # the app it's building
  "conflict ch10 14.0 22.0 1.5"   # Conflicts with main, Resolve fills the prompt
  "merge    ch10 33.4 39.4 1.0"   # hold to merge, merged
  "setup    ch11 15.0 23.5 1.5"   # setup failed, Fix in a new thread
  "agents   ch13 0.2  7.2  1.0"   # the four .agents/ files
  "daynight ch21 0.5  5.0  1.0"   # Day, Settings, Night
  "close    card 0    5.0  1.0"   # the sails and the tagline
)

card() {
  uv run --quiet --with pillow python - $root/design/icons/AppIcon.png $D/card.png <<'PY'
import sys
from PIL import Image, ImageDraw, ImageFont
icon, out = sys.argv[1], sys.argv[2]
W, H, plum = 1180, 780, (0x31, 0x1E, 0x36)
img = Image.new("RGB", (W, H), plum)
mark = Image.open(icon).convert("RGB").resize((360, 360), Image.LANCZOS)
# Only the sails: the icon's own plum ground is slightly noisy and would show as a square.
mask = Image.new("L", mark.size)
mask.putdata([max(0, min(255, (max(px) - 70) * 255 // 110)) for px in mark.getdata()])
img.paste(mark, ((W - 360) // 2, 120), mask)
draw = ImageDraw.Draw(img)
def font(size, weight):
    f = ImageFont.truetype("/System/Library/Fonts/SFNS.ttf", size)
    # SF's axes: width, optical size, grade, weight.
    f.set_variation_by_axes([100, min(size, 96), 400, weight])
    return f
for text, f, y, colour in [("Flotilla", font(68, 700), 500, (255, 255, 255)),
                           ("A fleet of coding agents, each in its own boat.", font(30, 400), 590, (0xC2, 0xA7, 0xE2))]:
    w = draw.textlength(text, font=f)
    draw.text(((W - w) / 2, y), text, font=f, fill=colour)
img.save(out)
PY
}

build() {
  mkdir -p $D/cuts
  card
  local list=$D/cuts/list.txt t=0 i=0 captions=""
  : > $list
  for s in $segments; do
    local parts=(${=s})
    local cap=${parts[1]} ch=${parts[2]} a=${parts[3]} b=${parts[4]} speed=${parts[5]}
    i=$((i + 1))
    local cut=$D/cuts/$(printf %02d $i).mp4
    if [[ $ch == card ]]; then
      ffmpeg -hide_banner -loglevel error -y -loop 1 -t $b -i $D/card.png -vf "fps=30,format=yuv420p" -c:v libx264 -crf 18 $cut
    else
      ffmpeg -hide_banner -loglevel error -y -ss $a -to $b -i $src/$ch.mp4 -an \
        -vf "crop=1180:780:0:0,setpts=PTS/$speed,fps=30,format=yuv420p" -c:v libx264 -crf 18 $cut
    fi
    local len=$(ffprobe -v error -show_entries format=duration -of csv=p=0 $cut)
    [[ $cap != - ]] && captions+="$(printf '%.3f\t%s' $(( t + 0.15 )) $cap)\n"
    t=$(( t + len ))
    echo "file '$cut'" >> $list
    printf "%-9s %s %5.2fs\n" $cap $ch $len
  done
  printf "%b%.3f\t\n" "$captions" $t > $D/captions.tsv
  ffmpeg -hide_banner -loglevel error -y -f concat -safe 0 -i $list -c copy $D/picture.mp4
  # Each line starts with its segment (a hair after the cut); none may run into the next.
  $narrate mix $D/captions.tsv $D/voice/narration.tsv $D/narration.wav
  local end=$(printf %.3f $(( t - 0.4 )))
  mkdir -p ${out:h}
  ffmpeg -hide_banner -loglevel error -y -i $D/picture.mp4 -i $D/narration.wav \
    -vf "fade=t=in:st=0:d=0.3,fade=t=out:st=$end:d=0.4" -af "afade=t=out:st=$end:d=0.4" \
    -c:v libx264 -preset slow -crf 23 -pix_fmt yuv420p -c:a aac -ac 1 -b:a 96k -shortest -movflags +faststart $out
  ffprobe -v error -show_entries format=duration,size -of default=nw=1 $out
}

case ${1:-} in
  voice) cd $root && NARRATOR=openrouter $narrate say $root/script/demo/short-demo.tsv $D/voice ;;
  build) build ;;
  *) print -u2 "usage: $0 voice | build"; exit 1 ;;
esac
