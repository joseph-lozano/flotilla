#!/bin/zsh
# script/demo/assemble.sh <recording dir> <out.mp4> [narration.wav]: after a FLOTILLA_SNAPSHOT_SCRIPT run that used
# rec:start / caption: / rec:stop, stack the app frames over caption strips into a 30 fps H.264 video. With a
# narration track (narrate.py mix), it goes under the video as AAC.
set -eu
dir=$1; out=$2; narration=${3:-}
w=$(python3 -c "import struct;d=open('$dir/'+open('$dir/frames.txt').readline().split(\"'\")[1],'rb').read(24);print(struct.unpack('>II',d[16:24])[0])")
mkdir -p $dir/caps
swift ${0:A:h}/captions.swift $dir/captions.tsv $dir/caps $w 64
# The recording's length (the last line of captions.tsv); the concat demuxer overshoots it.
len=$(tail -1 $dir/captions.tsv | cut -f1)
audio=()
[[ -n $narration ]] && audio=(-i ${narration:A} -map 2:a -c:a aac -b:a 160k)
cd $dir
ffmpeg -hide_banner -loglevel error -y \
  -f concat -safe 0 -i frames.txt \
  -f concat -safe 0 -i caps/caps.txt \
  ${audio[1,2]} \
  -filter_complex "[0:v]fps=30,format=yuv420p[a];[1:v]fps=30,format=yuv420p[b];[a][b]vstack=inputs=2:shortest=1[v]" \
  -map "[v]" ${audio[3,-1]} -t $len -c:v libx264 -crf 20 -preset medium -movflags +faststart $out
ffprobe -v error -show_entries format=duration:stream=width,height,codec_name -of default=nw=1 $out
