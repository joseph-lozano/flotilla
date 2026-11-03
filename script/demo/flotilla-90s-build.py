"""Cut and join the 90-second video from its takes (see flotilla-90s.sh).

    flotilla-90s-build.py <workdir> <out.mp4> [narration.tsv]

<workdir> holds the Mac takes (`runs/<take>/clip.mp4`, the snapshot recorder's frames at 30 fps),
the iPhone scenes (`phone/<scene>-fixed.mp4`, from simframes, with `<scene>.t0` and
`<scene>.touches`), and the drawn pieces (`art/`, from flotilla-90s-art.py). Each segment is
cut to 1180×780 at 30 fps; the iPhone's sit in a phone frame with a dot wherever a finger
touched. With narration.tsv (narrate.py take), each line starts with its segment; without it,
each segment's caption goes in a strip at the bottom instead.
"""

import os
import subprocess
import sys

W, H, FPS = 1180, 780, 30
SCREEN_W, SCREEN_H, BEZEL, PAD = 330, 716, 10, 60
# The simulator's screen in points.
PT_W, PT_H = 402, 874
# A logged tap lands this long after the test logs it (XCUITest waits for the app first).
TAP_LAG = 0.4

# key, kind, source, pieces (in, out, speed), caption (shown only without narration).
SEGMENTS = [
    # Four agents on real, different work in one take (Working 4 throughout): Opus 5.5 checking
    # the visualizer at three widths, then OpenCode on GPT-6 Luna writing the contributor guide's
    # Troubleshooting (Codex's view, between them, is cut).
    ("open", "montage", None, [("fleet-a", 0.2, 4.6, 1.0), ("fleet-a", 8.8, 13.8, 1.0)], "Each agent alone in its own cloud VM"),
    # New thread: the task typed, the model picker (Claude Code, then Codex above), GPT-6.1 Sol, Start.
    ("new", "mac", "live", [(0.4, 11.3, 1.6)], "Claude Code, Codex or OpenCode"),
    # Its start: the branch named, a VM, the latest main, the repo's setup, then the turn running.
    ("start", "mac", "live", [(11.3, 40.0, 7.0), (40.0, 43.0, 1.3)], "A fresh VM in seconds"),
    # Preview's menu (web and info ready), then the page it opens (the real dev server, no address bar).
    ("preview", "montage", None, [("preview", 0.3, 4.6, 1.3), ("browser", 0.0, 2.8, 1.0)], "Preview the VM's dev server"),
    # Make public… for 1 hour, the toast, then the menu with "Public · 60 min left".
    ("public", "mac", "preview", [(5.3, 14.8, 1.55)], "Make public, for a time"),
    # The pull request with a failing check, a follow-up asking for the changelog line, the push.
    ("merge", "mac", "merge", [(0.3, 6.8, 1.45), (6.8, 56.5, 14.0)], "The pull request on the thread"),
    # Both checks pass; the hold in real time, Merging… at double speed, then Merged.
    ("hold", "mac", "merge", [(57.8, 60.2, 2.0), (60.2, 62.6, 1.0), (62.6, 65.6, 2.0), (65.6, 66.6, 1.0)], "Hold to merge"),
    # Opus 5.5's thread: the Codex (GPT-6.1 Sol) and Claude Code (Fable 5.1) reviewer cards,
    # "2 agents answered", then who caught what.
    ("handoff", "mac", "handoff", [(0.4, 7.3, 1.7), (7.3, 12.9, 1.35)], "Agents hand off work"),
    # A stopped VM (grey dot, Resume & send), a follow-up, the agent answering.
    ("idle", "mac", "wake", [(0.4, 6.9, 1.5), (6.9, 27.6, 6.5), (27.6, 29.6, 1.0)], "Idle VMs stop and wake"),
    # Send later from New thread: Repeat, Weekdays, 09:00, Schedule, then the Scheduled list.
    ("schedule", "mac", "schedule", [(0.3, 4.4, 2.8), (4.4, 12.6, 1.85), (12.6, 14.4, 1.0)], "Prompts on a schedule"),
    # Claude Code through `flotilla mcp` (its real stream, drawn over the Mac's take): it starts a
    # Haiku thread, which appears in the sidebar, and reads its answer.
    ("mcp", "mac", "mcpcomp", [(0.2, 7.3, 2.4), (7.3, 21.0, 2.3), (21.0, 22.2, 1.0)], "Other assistants use Flotilla"),
    # A short beat: the last frame dissolves into the backdrop, and the iPhone rises into frame
    # alone: its Threads list with the agents' timers ticking (the end of the pairing take,
    # between the notifications prompt and the test closing, a little slowed).
    ("pause", "pause", None, None, ""),
    ("iphone", "reveal-part", ("pair", 25.0, 28.2, 1.0, 1.0, 0.8), (0.0, 4.4), "Flotilla on the iPhone"),
    # The Mac slides in beside it on Codex's waiting question: the phone picks the first option and
    # sends it, the Mac shows it answered at once; then the Mac slides out, the phone to the middle.
    ("ask", "side-both", "ask", [(0.3, 6.6, 1.0)], "Answer an agent from either"),
    # The phone alone: Hold to merge by touch in real time, Merging… at double speed, merged.
    ("pocket", "phone", "merge", [(26.6, 29.0, 1.0), (29.0, 31.7, 2.0), (31.7, 32.6, 1.0)], "The desktop app's features, in your pocket"),
    ("close", "card", None, None, ""),
]


def run(args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def length(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path], capture_output=True, text=True, check=True)
    return float(out.stdout.strip())


def pieces_filter(label, pieces, extra=""):
    """A filter that cuts `pieces` out of input `label` (each at its speed) and joins them."""
    parts, names = [], []
    for i, (a, b, s) in enumerate(pieces):
        name = f"p{i}"
        parts.append(f"[{label}]trim={a}:{b},setpts=(PTS-STARTPTS)/{s},fps={FPS}{extra}[{name}]")
        names.append(f"[{name}]")
    if len(pieces) == 1:
        return ";".join(parts).rsplit("[", 1)[0] + "[cut]"
    return ";".join(parts) + ";" + "".join(names) + f"concat=n={len(pieces)}:v=1:a=0[cut]"


ENC = ["-c:v", "libx264", "-crf", "15", "-preset", "medium", "-pix_fmt", "yuv420p", "-r", str(FPS)]


def mac(work, out, take, pieces):
    src = os.path.join(work, "runs", take, "clip.mp4")
    run(["-i", src, "-filter_complex", pieces_filter("0:v", pieces), "-map", "[cut]", "-an", *ENC, out])


def touches(work, scene):
    """The scene's touches: (time in the recording, x, y in points, seconds held)."""
    t0 = float(open(os.path.join(work, "phone", f"{scene}.t0")).read())
    found = []
    path = os.path.join(work, "phone", f"{scene}.touches")
    for line in open(path) if os.path.exists(path) else []:
        t, x, y, hold = (float(v) for v in line.split("\t"))
        if hold >= 0:
            found.append((t - t0 + TAP_LAG, x, y, hold))
    return found


def dots(work, scene, pieces, sx, sy, first_input):
    """Overlay filters putting a dot on each touch the pieces keep, in the cut's time."""
    filters, inputs, n = [], [], first_input
    offset = 0.0
    for a, b, s in pieces:
        for t, x, y, hold in touches(work, scene):
            if a <= t < b:
                start = offset + (t - a) / s
                end = start + (max(hold, 0.3) / s)
                png = "dot-hold.png" if hold > 0 else "dot.png"
                size = 60 if hold > 0 else 52
                px = sx + x * SCREEN_W / PT_W - size / 2
                py = sy + y * SCREEN_H / PT_H - size / 2
                inputs += ["-i", os.path.join(work, "art", png)]
                filters.append((n, px, py, start, end))
                n += 1
        offset += (b - a) / s
    return inputs, filters


def phone_layer(label, scene_pieces, sx, sy):
    """Filters scaling the screen cut and putting it, rounded, at (sx, sy): numbers, or ffmpeg
    expressions in t (a phone that moves)."""
    bx = sx - BEZEL - PAD if not isinstance(sx, str) else f"({sx})-{BEZEL + PAD}"
    by = sy - BEZEL - PAD if not isinstance(sy, str) else f"({sy})-{BEZEL + PAD}"
    return (f"[cut]scale={SCREEN_W}:{SCREEN_H},format=rgba[ph];[mask]format=gray,scale={SCREEN_W}:{SCREEN_H}[m];[ph][m]alphamerge[phm];"
            f"[{label}][phm]overlay=x='{sx}':y='{sy}':shortest=1[l1];[l1][bezel]overlay=x='{bx}':y='{by}'[l2]")


def ease(d):
    """An ease-out over the first `d` seconds of a segment, 0 to 1, as an ffmpeg expression."""
    return f"(1-pow(1-min(t/{d},1),3))"


def montage(work, out, pieces):
    """Pieces (take, in, out, speed) of several Mac takes, joined in order."""
    inputs, parts = [], []
    for i, (take, a, b, sp) in enumerate(pieces):
        inputs += ["-i", os.path.join(work, "runs", take, "clip.mp4")]
        parts.append(f"[{i}:v]trim={a}:{b},setpts=(PTS-STARTPTS)/{sp},fps={FPS}[p{i}]")
    f = ";".join(parts) + ";" + "".join(f"[p{i}]" for i in range(len(pieces))) + f"concat=n={len(pieces)}:v=1:a=0[cut]"
    run([*inputs, "-filter_complex", f, "-map", "[cut]", "-an", *ENC, out])


def phone(work, out, scene, pieces):
    art = os.path.join(work, "art")
    src = os.path.join(work, "phone", f"{scene}-fixed.mp4")
    sx, sy = (W - SCREEN_W) // 2, (H - SCREEN_H) // 2
    total = sum((b - a) / s for a, b, s in pieces)
    extra_inputs, dot_list = dots(work, scene, pieces, sx, sy, 4)
    f = pieces_filter("1:v", pieces) + ";[2:v]copy[mask];[3:v]copy[bezel];"
    f += phone_layer("0:v", pieces, sx, sy)
    last = "l2"
    for i, (n, px, py, a, b) in enumerate(dot_list):
        f += f";[{last}][{n}:v]overlay={px:.0f}:{py:.0f}:enable='between(t,{a:.3f},{b:.3f})'[d{i}]"
        last = f"d{i}"
    run(["-loop", "1", "-t", f"{total:.3f}", "-i", os.path.join(art, "backdrop.png"), "-i", src,
         "-i", os.path.join(art, "phone-mask.png"), "-i", os.path.join(art, "phone-bezel.png"), *extra_inputs,
         "-filter_complex", f, "-map", f"[{last}]", "-an", *ENC, "-t", f"{total:.3f}", out])


def side(work, out, pieces, scene, enter=0.0, leave=0.0):
    """The Mac's thread pane and the iPhone side by side, both recorded at once: the Mac's frames
    from runs/<scene> (its recorder notes when it started, in rec-start.txt), the phone's from
    phone/<scene>, lined up on that clock, cut into the same pieces at the same speeds, with a
    dot wherever a finger touched the phone."""
    art = os.path.join(work, "art")
    mac_src = os.path.join(work, "runs", scene, "clip.mp4")
    ph_src = os.path.join(work, "phone", f"{scene}-fixed.mp4")
    offset = float(open(os.path.join(work, "runs", scene, "rec-start.txt")).read()) - float(open(os.path.join(work, "phone", f"{scene}.t0")).read())
    total = sum((b - a) / s for a, b, s in pieces)
    # The thread pane (right of the sidebar), 900×780, at 730×632.
    mw, mh = 730, 632
    mx, my = 35, (H - mh) // 2
    sx, sy = mx + mw + 50, (H - SCREEN_H) // 2
    # With `enter`, the Mac's window slides in from the left while the phone glides from the
    # middle (where the reveal left it) to its place.
    cx = (W - SCREEN_W) // 2
    total_t = sum((b - a) / s_ for a, b, s_ in pieces)
    come = f"{ease(enter)}" if enter else "1"
    # Leaving: smoothstep over the last `leave` seconds, 0 to 1.
    go = f"(pow(max(0,min((t-{total_t - leave:.3f})/{leave},1)),2)*(3-2*max(0,min((t-{total_t - leave:.3f})/{leave},1))))" if leave else "0"
    out_by = f"({mw + mx + 2 * PAD})"
    mac_x = (lambda x: f"{x}-{out_by}*(1-{come})-{out_by}*{go}") if (enter or leave) else (lambda x: x)
    ph_x = f"{cx}+{sx - cx}*{come}-{sx - cx}*{go}" if (enter or leave) else sx
    mac_p = [(a, b, s) for a, b, s in pieces]
    ph_p = [(a + offset, b + offset, s) for a, b, s in pieces]
    f = pieces_filter("1:v", mac_p, ",crop=900:780:280:0,scale=%d:%d" % (mw, mh)).replace("[cut]", "[mac]").replace("[p", "[m")
    f += ";" + pieces_filter("2:v", ph_p) + ";"
    f += (f"[mac]format=rgba[maca];[4:v]format=gray,scale={mw}:{mh}[mm];[maca][mm]alphamerge[macr];"
          f"[0:v][5:v]overlay=x='{mac_x(mx - PAD)}':y={my - PAD}[g0];[g0][macr]overlay=x='{mac_x(mx)}':y={my}[g1];"
          f"[3:v]copy[mask];[6:v]copy[bezel];")
    f += phone_layer("g1", None, ph_x, sy)
    # A dot wherever a finger touched the phone, in the cut's time.
    extra_inputs, dot_list = dots(work, scene, ph_p, sx, sy, 7)
    last = "l2"
    for i, (n, px, py, a, b) in enumerate(dot_list):
        f += f";[{last}][{n}:v]overlay={px:.0f}:{py:.0f}:enable='between(t,{a:.3f},{b:.3f})'[d{i}]"
        last = f"d{i}"
    run(["-loop", "1", "-t", f"{total:.3f}", "-i", os.path.join(art, "backdrop.png"), "-i", mac_src, "-i", ph_src,
         "-i", os.path.join(art, "phone-mask.png"), "-i", os.path.join(art, "mac-sync-mask.png"), "-i", os.path.join(art, "mac-sync-shadow.png"),
         "-i", os.path.join(art, "phone-bezel.png"), *extra_inputs,
         "-filter_complex", f, "-map", f"[{last}]", "-an", *ENC, "-t", f"{total:.3f}", out])


def reveal(work, out, scene, a, b, rise=1.0, hold=0.4, speed=1.0):
    """The phone alone on the backdrop: it rises into the middle of the frame (its screen held on
    the take's frame at `a`), settles, then the take plays from `a` to `b` and holds."""
    art = os.path.join(work, "art")
    src = os.path.join(work, "phone", f"{scene}-fixed.mp4")
    sx, sy = (W - SCREEN_W) // 2, (H - SCREEN_H) // 2
    total = rise + (b - a) / speed + hold
    y = f"{sy}+{H - sy + 40}*(1-{ease(rise)})"
    f = (f"[1:v]trim={a}:{b},setpts=(PTS-STARTPTS)/{speed},fps={FPS},tpad=start_mode=clone:start_duration={rise}:stop_mode=clone:stop_duration={hold}[cut];"
         f"[2:v]copy[mask];[3:v]copy[bezel];")
    f += phone_layer("0:v", None, sx, y)
    run(["-loop", "1", "-t", f"{total:.3f}", "-i", os.path.join(art, "backdrop.png"), "-i", src,
         "-i", os.path.join(art, "phone-mask.png"), "-i", os.path.join(art, "phone-bezel.png"),
         "-filter_complex", f, "-map", "[l2]", "-an", *ENC, "-t", f"{total:.3f}", out])


def pause(work, out, prev, secs=0.2, fade=0.18):
    """A short beat: the last frame of the cut before dissolving into the backdrop (where the
    phone rises next)."""
    still = out + ".last.png"
    run(["-sseof", "-0.05", "-i", prev, "-frames:v", "1", "-update", "1", still])
    run(["-loop", "1", "-t", f"{secs:.3f}", "-i", still, "-loop", "1", "-t", f"{secs:.3f}", "-i", os.path.join(work, "art", "backdrop.png"),
         "-filter_complex", f"[1:v]format=rgba,fade=t=in:st=0:d={fade}:alpha=1[b];[0:v][b]overlay=0:0,format=yuv420p[v]", "-map", "[v]", *ENC, "-t", f"{secs:.3f}", out])


def reveal_part(work, out, args, a, b):
    """Seconds a to b of the phone's reveal (reveal() with `args`), rendered once to runs/reveal."""
    full = os.path.join(work, "runs", "reveal", "clip.mp4")
    os.makedirs(os.path.dirname(full), exist_ok=True)
    stamp = full + ".args"
    if not os.path.exists(full) or not os.path.exists(stamp) or open(stamp).read() != repr(args):
        reveal(work, full, *args)
        open(stamp, "w").write(repr(args))
    mac(work, out, "reveal", [(a, b, 1.0)])


def frames(work, out, name):
    """A drawn piece, art/<name>/NNNN.png at 30 fps."""
    run(["-framerate", str(FPS), "-i", os.path.join(work, "art", name, "%04d.png"), *ENC, out])


def term(work, out):
    run(["-framerate", str(FPS), "-i", os.path.join(work, "art", "term", "%04d.png"), *ENC, out])


def card(work, out, secs=4.7):
    run(["-loop", "1", "-t", str(secs), "-i", os.path.join(work, "art", "card.png"), *ENC, out])


def build(work, out, narration=None):
    cuts = os.path.join(work, "cuts")
    os.makedirs(cuts, exist_ok=True)
    listing, captions, t = [], [], 0.0
    prev = None
    for i, (key, kind, src, pieces, caption) in enumerate(SEGMENTS):
        path = os.path.join(cuts, f"{i:02}-{key}.mp4")
        {"mac": lambda: mac(work, path, src, pieces), "phone": lambda: phone(work, path, src, pieces),
         "side": lambda: side(work, path, pieces, src), "montage": lambda: montage(work, path, pieces),
         "side-enter": lambda: side(work, path, pieces, src, enter=0.9), "reveal": lambda: reveal(work, path, src, *pieces),
         "pause": lambda: pause(work, path, prev), "frames": lambda: frames(work, path, src),
         "side-both": lambda: side(work, path, pieces, src, enter=0.9, leave=0.7),
         "reveal-part": lambda: reveal_part(work, path, src, *pieces),
         "term": lambda: term(work, path), "card": lambda: card(work, path)}[kind]()
        secs = length(path)
        prev = path
        captions.append((t, key, caption))
        print(f"{key:11} {secs:5.2f}s  at {t:5.2f}s")
        t += secs
        listing.append(f"file '{path}'")
    open(os.path.join(cuts, "list.txt"), "w").write("\n".join(listing) + "\n")
    picture = os.path.join(work, "picture.mp4")
    run(["-f", "concat", "-safe", "0", "-i", os.path.join(cuts, "list.txt"), "-c", "copy", picture])
    # Each line starts a hair after its segment does.
    with open(os.path.join(work, "captions.tsv"), "w") as f:
        for start, key, _ in captions:
            f.write(f"{start + 0.2:.3f}\t{key}\n")
        f.write(f"{t:.3f}\t\n")
    end = t - 0.5
    os.makedirs(os.path.dirname(out), exist_ok=True)
    if narration:
        here = os.path.dirname(os.path.abspath(__file__))
        wav = os.path.join(work, "narration.wav")
        subprocess.run(["uv", "run", "--quiet", "--python", "3.13", "--with", "mlx-audio==0.4.2", "--with", "soundfile", "python",
                        os.path.join(here, "narrate.py"), "mix", os.path.join(work, "captions.tsv"), narration, wav], check=True)
        # Peaks only: a lookahead limiter at 4x the rate (inter-sample peaks), so the encoded
        # file stays under -1 dBTP; the constant gain (narrate.py's -16 LUFS) is untouched.
        limited = os.path.join(work, "narration-tp.wav")
        ceiling = float(os.environ.get("TP_CEILING", "-1.5"))
        run(["-i", wav, "-af", f"aresample=96000,alimiter=limit={10 ** (ceiling / 20):.4f}:attack=1:release=60:level=false:latency=true,aresample=24000",
             "-c:a", "pcm_f32le", limited])
        wav = limited
        run(["-i", picture, "-i", wav, "-vf", f"fade=t=in:st=0:d=0.3,fade=t=out:st={end:.3f}:d=0.5", "-af", f"afade=t=out:st={end:.3f}:d=0.5",
             "-c:v", "libx264", "-preset", "slow", "-crf", "12", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "1", "-b:a", "160k",
             "-shortest", "-movflags", "+faststart", out])
    else:
        # No voice: the captions, in a strip over the bottom.
        here = os.path.dirname(os.path.abspath(__file__))
        caps = os.path.join(work, "caps")
        os.makedirs(caps, exist_ok=True)
        with open(os.path.join(work, "captions-text.tsv"), "w") as f:
            for start, _, caption in captions:
                f.write(f"{start:.3f}\t{caption}\n")
            f.write(f"{t:.3f}\t\n")
        subprocess.run(["swift", os.path.join(here, "captions.swift"), os.path.join(work, "captions-text.tsv"), caps, str(W), "56"], check=True, capture_output=True)
        run(["-i", picture, "-f", "concat", "-safe", "0", "-i", os.path.join(caps, "caps.txt"),
             "-filter_complex", f"[1:v]fps={FPS},format=rgba,colorchannelmixer=aa=0.86[c];[0:v][c]overlay=0:H-h:shortest=1,fade=t=in:st=0:d=0.3,fade=t=out:st={end:.3f}:d=0.5[v]",
             "-map", "[v]", "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", out])
    print(f"{out}: {length(out):.2f}s")


if __name__ == "__main__":
    build(sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
