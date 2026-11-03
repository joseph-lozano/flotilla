"""The 90-second video's drawn pieces, at 1180×780 (the Mac window's size):

    flotilla-90s-art.py <outdir> <icon.png> <terminal.txt>

- `backdrop.png`: the ground the phone and the terminal sit on.
- `phone-mask.png`, `phone-bezel.png`: the phone's rounded screen and its frame, for a
  recording scaled to SCREEN_H tall (see `phone_at`).
- `dot.png`, `dot-hold.png`: a touch, and a touch that's held.
- `term/NNNN.png` (30 fps): the always-on box's terminal: `systemctl --user status flotilla`,
  then `flotilla doctor`, typed and answered. Its text is terminal.txt: real output, with the
  user, machine, tailnet and GitHub account made generic (you@box, box.tail1234.ts.net, acme),
  and the key's last characters, the unit's Docs link (it names the repo's owner) and the unset
  Push line taken out by hand.
- `card.png`: the end card.
"""

import os
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H = 1180, 780
PLUM = (0x31, 0x1E, 0x36)
SURFACE = (0xF2, 0xEC, 0xF4)
LAVENDER = (0xC2, 0xA7, 0xE2)
CORAL = (0xF3, 0x71, 0x73)
AMBER = (0xF6, 0xBC, 0x63)
GREEN = (0x7C, 0xD6, 0x8F)

# The phone: the simulator's 1206×2622 screen, scaled to this height.
SCREEN_H = 716
SCREEN_W = round(SCREEN_H * 1206 / 2622 / 2) * 2
BEZEL = 10
RADIUS = round(55 * SCREEN_H / 874)


def sf(size, weight=400):
    f = ImageFont.truetype("/System/Library/Fonts/SFNS.ttf", size)
    f.set_variation_by_axes([100, min(size, 96), 400, weight])
    return f


def mono(size):
    return ImageFont.truetype("/System/Library/Fonts/SFNSMono.ttf", size)


def backdrop(out):
    # Lavender at the top to the site's surface colour at the bottom, with no edges to see.
    top, bottom = (0xEA, 0xE0, 0xF1), SURFACE
    img = Image.new("RGB", (W, H))
    d = ImageDraw.Draw(img)
    for y in range(H):
        k = y / (H - 1)
        d.line((0, y, W, y), fill=tuple(round(a + (b - a) * k) for a, b in zip(top, bottom)))
    img.save(os.path.join(out, "backdrop.png"))
    return img


def phone_parts(out):
    sw, sh = SCREEN_W, SCREEN_H
    mask = Image.new("L", (sw, sh), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, sw - 1, sh - 1), RADIUS, fill=255)
    mask.save(os.path.join(out, "phone-mask.png"))
    bw, bh = sw + 2 * BEZEL, sh + 2 * BEZEL
    pad = 60
    bezel = Image.new("RGBA", (bw + 2 * pad, bh + 2 * pad), (0, 0, 0, 0))
    # A shadow under the phone.
    shadow = Image.new("L", bezel.size, 0)
    ImageDraw.Draw(shadow).rounded_rectangle((pad + 6, pad + 22, pad + bw - 6, pad + bh + 6), RADIUS + BEZEL, fill=95)
    shadow = shadow.filter(ImageFilter.GaussianBlur(22))
    bezel = Image.composite(Image.new("RGBA", bezel.size, (0x31, 0x1E, 0x36, 255)), bezel, shadow)
    ring = Image.new("RGBA", bezel.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(ring)
    d.rounded_rectangle((pad, pad, pad + bw - 1, pad + bh - 1), RADIUS + BEZEL, fill=(0x22, 0x1A, 0x26, 255))
    d.rounded_rectangle((pad + 1, pad + 1, pad + bw - 2, pad + bh - 2), RADIUS + BEZEL - 1, outline=(0x4A, 0x3D, 0x50, 255), width=2)
    # The screen is a hole.
    hole = Image.new("L", bezel.size, 0)
    ImageDraw.Draw(hole).rounded_rectangle((pad + BEZEL, pad + BEZEL, pad + BEZEL + sw - 1, pad + BEZEL + sh - 1), RADIUS, fill=255)
    ring.putalpha(Image.composite(Image.new("L", bezel.size, 0), ring.getchannel("A"), hole))
    bezel.alpha_composite(ring)
    # Cut the shadow out of the screen too: the recording goes there.
    alpha = bezel.getchannel("A")
    bezel.putalpha(Image.composite(Image.new("L", bezel.size, 0), alpha, hole))
    bezel.save(os.path.join(out, "phone-bezel.png"))
    # The Mac's window beside the phone, rounded, with a shadow: the whole window for Add device
    # (732×484), its thread pane for the side-by-side (730×632).
    for suffix, (mw, mh) in [("", (732, 484)), ("-sync", (730, 632))]:
        mmask = Image.new("L", (mw, mh), 0)
        ImageDraw.Draw(mmask).rounded_rectangle((0, 0, mw - 1, mh - 1), 10, fill=255)
        mmask.save(os.path.join(out, f"mac{suffix}-mask.png"))
        msh = Image.new("L", (mw + 2 * pad, mh + 2 * pad), 0)
        ImageDraw.Draw(msh).rounded_rectangle((pad + 6, pad + 16, pad + mw - 6, pad + mh + 4), 12, fill=80)
        msh = msh.filter(ImageFilter.GaussianBlur(18))
        shade = Image.new("RGBA", msh.size, (0x31, 0x1E, 0x36, 255))
        shade.putalpha(msh)
        shade.save(os.path.join(out, f"mac{suffix}-shadow.png"))
    for name, r, fill in [("dot.png", 22, 150), ("dot-hold.png", 26, 170)]:
        size = 2 * r + 8
        dot = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        dd = ImageDraw.Draw(dot)
        dd.ellipse((4, 4, size - 5, size - 5), fill=(255, 255, 255, fill), outline=(0x31, 0x1E, 0x36, 120), width=2)
        dot.save(os.path.join(out, name))


def terminal(out, text_path):
    lines = open(text_path).read().rstrip("\n").split("\n")
    frames = os.path.join(out, "term")
    os.makedirs(frames, exist_ok=True)
    ground = Image.open(os.path.join(out, "backdrop.png")).convert("RGB")
    tw, th = 1040, 560
    tx, ty = (W - tw) // 2, (H - th) // 2 + 6
    font, bold = mono(15), mono(15)
    title = sf(13, 600)
    prompt = "you@box:~$ "
    # Each line is shown from a time: commands type at ~22 characters a second.
    script = []  # (kind, text, start, end)
    t = 0.6
    for line in lines:
        if line.startswith("$ "):
            cmd = line[2:]
            script.append(("cmd", cmd, t, t + len(cmd) / 22))
            t += len(cmd) / 22 + 0.45
        else:
            script.append(("out", line, t, t))
            t += 0.05 if not line.startswith("✓") else 0.22
    total = t + 2.2
    line_h = 22
    for n in range(int(total * 30)):
        now = n / 30
        img = ground.copy()
        # Window shadow, body, title bar.
        sh = Image.new("L", (W, H), 0)
        ImageDraw.Draw(sh).rounded_rectangle((tx + 8, ty + 20, tx + tw - 8, ty + th + 8), 14, fill=90)
        sh = sh.filter(ImageFilter.GaussianBlur(20))
        img = Image.composite(Image.new("RGB", (W, H), PLUM), img, sh)
        d = ImageDraw.Draw(img)
        d.rounded_rectangle((tx, ty, tx + tw, ty + th), 12, fill=(0x1E, 0x15, 0x22))
        d.rounded_rectangle((tx, ty, tx + tw, ty + 34), 12, fill=(0x2C, 0x20, 0x31))
        d.rectangle((tx, ty + 22, tx + tw, ty + 34), fill=(0x2C, 0x20, 0x31))
        for i, c in enumerate([CORAL, AMBER, (0x8F, 0xC9, 0x7E)]):
            d.ellipse((tx + 16 + i * 20, ty + 11, tx + 28 + i * 20, ty + 23), fill=c)
        label = "you@box: ~ — ssh"
        lw = d.textlength(label, font=title)
        d.text((tx + (tw - lw) / 2, ty + 9), label, font=title, fill=(0xC9, 0xBC, 0xCF))
        y = ty + 52
        cursor_at = None
        for kind, text, start, end in script:
            if now < start:
                break
            x = tx + 22
            if kind == "cmd":
                d.text((x, y), prompt, font=bold, fill=GREEN)
                x += d.textlength(prompt, font=bold)
                shown = text[: int(len(text) * min(1, (now - start) / max(end - start, 0.01)))]
                d.text((x, y), shown, font=font, fill=(0xF4, 0xEE, 0xF6))
                cursor_at = (x + d.textlength(shown, font=font), y) if now < end + 0.45 else None
            else:
                colour = (0xD9, 0xCF, 0xDD)
                if text.startswith("●"):
                    d.text((x, y), "●", font=font, fill=GREEN)
                    x += d.textlength("● ", font=font)
                    text = text[2:]
                    colour = (0xF4, 0xEE, 0xF6)
                elif text.startswith("✓"):
                    d.text((x, y), "✓", font=font, fill=GREEN)
                    x += d.textlength("✓ ", font=font)
                    text = text[2:]
                if "active (running)" in text:
                    a, b = text.split("active (running)", 1)
                    d.text((x, y), a, font=font, fill=colour)
                    x += d.textlength(a, font=font)
                    d.text((x, y), "active (running)", font=font, fill=GREEN)
                    x += d.textlength("active (running)", font=font)
                    text = b
                d.text((x, y), text, font=font, fill=colour)
            y += line_h
        if cursor_at is None and now > script[-1][2]:
            # The prompt again, waiting.
            d.text((tx + 22, y), prompt, font=bold, fill=GREEN)
            cursor_at = (tx + 22 + d.textlength(prompt, font=bold), y)
        if cursor_at and int(now * 2) % 2 == 0:
            d.rectangle((cursor_at[0] + 1, cursor_at[1] + 2, cursor_at[0] + 9, cursor_at[1] + 19), fill=(0xC2, 0xA7, 0xE2))
        img.save(os.path.join(frames, f"{n:04}.png"))
    print(f"terminal: {total:.1f}s")


def card(out, icon):
    img = Image.new("RGB", (W, H), PLUM)
    mark = Image.open(icon).convert("RGB").resize((300, 300), Image.LANCZOS)
    # Only the sails: the icon's own plum ground is slightly noisy and would show as a square.
    mask = Image.new("L", mark.size)
    mask.putdata([max(0, min(255, (max(px) - 70) * 255 // 110)) for px in mark.get_flattened_data()])
    img.paste(mark, ((W - 300) // 2, 110), mask)
    d = ImageDraw.Draw(img)
    # The name, the one-liner, then where to get it.
    for text, f, y, colour in [
        ("Flotilla", sf(68, 700), 420, (255, 255, 255)),
        ("A fleet of coding agents, each in its own boat.", sf(28, 500), 512, LAVENDER),
        ("useflotilla.com", sf(32, 650), 578, (255, 255, 255)),
        ("Free and open source", sf(22, 500), 628, LAVENDER),
    ]:
        w = d.textlength(text, font=f)
        d.text(((W - w) / 2, y), text, font=f, fill=colour)
    img.save(os.path.join(out, "card.png"))


def mcp_session(out, stream_path, t0_path, rec_path, secs=26.0):
    """`term-mcp/NNNN.png` (30 fps, RGBA, 1180×780, clear outside the window): Claude Code using
    Flotilla through `flotilla mcp`, drawn over the Mac's take of the same moment (its main pane;
    the sidebar stays in view). Its lines come from Claude Code's own stream (stream.jsonl from
    `flotilla-90s.sh mcp`, each event with the time it arrived), shown in the Mac recording's
    time. The command shown is the one the docs give (`claude mcp add flotilla -- flotilla mcp`)
    and the request as asked; the scratch run's own paths and flags aren't shown."""
    import json
    import textwrap

    t0 = float(open(t0_path).read())
    rec = float(open(rec_path).read())
    shift = t0 - rec  # stream time to Mac recording time
    events = []  # (time, kind, text)
    ask = None
    for line in open(stream_path):
        d = json.loads(line)
        e, t = d["e"], d["t"] - t0 + shift
        if e.get("type") == "system" and e.get("subtype") == "init":
            events.append((t, "ready", ""))
        elif e.get("type") == "assistant":
            for c in e["message"]["content"]:
                if c["type"] == "tool_use":
                    name = c["name"].split("__")[-1]
                    args = ", ".join(f'{k}: {json.dumps(v)}' for k, v in c["input"].items() if k != "timeout_seconds")
                    events.append((t, "call", f"flotilla · {name} ({args})"))
                elif c["type"] == "text":
                    # The answer, without the sign-off Claude Code adds from its user's own settings.
                    text = c["text"].split("\n\n")[0]
                    events.append((t, "say", text))
        elif e.get("type") == "user":
            for c in e["message"]["content"]:
                if c.get("type") == "tool_result":
                    txt = c["content"] if isinstance(c["content"], str) else " ".join(x.get("text", "") for x in c["content"])
                    lines = [l for l in txt.split("\n") if l.strip()]
                    if lines and lines[0].startswith("claude-code (default"):
                        lines = [lines[0], f"… {len(lines) - 1} models"]
                    if lines and "turn has ended" in lines[0]:
                        lines = [lines[0]] + [l for l in lines[1:] if not l.startswith("Reply [")]
                    events.append((t, "result", "\n".join(lines[:3])))
    frames = os.path.join(out, "term-mcp")
    os.makedirs(frames, exist_ok=True)
    tx, ty, tw, th = 300, 132, 862, 560
    font, small = mono(14), mono(13)
    title = sf(13, 600)
    prompt = "you@mac:~$ "
    cmd1 = "claude mcp add flotilla -- flotilla mcp"
    request = os.environ.get("MCP_SHOWN", "Use Flotilla to start a thread on Haiku 5.5 in a blank VM that asks which Python version it has. Wait for its answer, then tell me what it said.")
    cmd2 = f'claude "{request}"'
    wrap = 92
    for n in range(int(secs * 30)):
        now = n / 30
        img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
        sh = Image.new("L", (W, H), 0)
        ImageDraw.Draw(sh).rounded_rectangle((tx + 8, ty + 18, tx + tw - 8, ty + th + 8), 14, fill=110)
        sh = sh.filter(ImageFilter.GaussianBlur(18))
        shade = Image.new("RGBA", (W, H), PLUM + (255,))
        shade.putalpha(sh)
        img.alpha_composite(shade)
        d = ImageDraw.Draw(img)
        d.rounded_rectangle((tx, ty, tx + tw, ty + th), 12, fill=(0x1E, 0x15, 0x22, 255))
        d.rounded_rectangle((tx, ty, tx + tw, ty + 34), 12, fill=(0x2C, 0x20, 0x31, 255))
        d.rectangle((tx, ty + 22, tx + tw, ty + 34), fill=(0x2C, 0x20, 0x31, 255))
        for i, c in enumerate([CORAL, AMBER, (0x8F, 0xC9, 0x7E)]):
            d.ellipse((tx + 16 + i * 20, ty + 11, tx + 28 + i * 20, ty + 23), fill=c)
        label = "you@mac: ~ — claude"
        lw = d.textlength(label, font=title)
        d.text((tx + (tw - lw) / 2, ty + 9), label, font=title, fill=(0xC9, 0xBC, 0xCF))
        x0, y = tx + 20, ty + 50
        lh = 21
        text = (0xF4, 0xEE, 0xF6)
        dim = (0xA8, 0x9C, 0xAE)
        cursor = None
        # The two commands, typed at ~40 characters a second from the start.
        typed = int(now * 40)
        for cmd in (cmd1, cmd2):
            shown = cmd[: max(0, typed)]
            typed -= len(cmd) + 12
            if not shown:
                break
            parts = textwrap.wrap(shown, wrap - len(prompt), drop_whitespace=False) or [""]
            d.text((x0, y), prompt, font=font, fill=GREEN)
            for k, part in enumerate(parts):
                xx = x0 + (d.textlength(prompt, font=font) if k == 0 else d.textlength(" " * len(prompt), font=font))
                d.text((xx, y), part, font=font, fill=text)
                cursor = (xx + d.textlength(part, font=font), y)
                y += lh
            if len(shown) < len(cmd):
                break
            cursor = None
            y += 6
        for t, kind, body in events:
            if now < t:
                break
            if kind == "ready":
                continue
            if kind == "call":
                for k, part in enumerate(textwrap.wrap(body, wrap - 2)):
                    if k == 0:
                        d.text((x0, y), "●", font=font, fill=LAVENDER)
                    d.text((x0 + 18, y), part, font=font, fill=text)
                    y += lh
            elif kind == "result":
                for k, raw in enumerate(body.split("\n")):
                    for j, part in enumerate(textwrap.wrap(raw, wrap - 6) or [""]):
                        lead = "└ " if k == 0 and j == 0 else "  "
                        d.text((x0 + 18, y), lead + part, font=small, fill=dim)
                        y += lh - 2
                y += 4
            elif kind == "say":
                for k, part in enumerate(textwrap.wrap(body, wrap - 2)):
                    if k == 0:
                        d.text((x0, y), "●", font=font, fill=GREEN)
                    d.text((x0 + 18, y), part, font=font, fill=text)
                    y += lh
                y += 4
        if cursor and int(now * 2) % 2 == 0:
            d.rectangle((cursor[0] + 1, cursor[1] + 2, cursor[0] + 9, cursor[1] + 19), fill=LAVENDER)
        img.save(os.path.join(frames, f"{n:04}.png"))
    print(f"mcp session: {secs:.1f}s, {len(events)} events")


def browser(out, page_path, secs=2.8):
    """`browser/NNNN.png` (30 fps): the preview's page as the Mac's browser shows it, in a window
    with its tab title and no address bar (the VM's address stays off screen), scrolling down a
    little. The page is a capture of the thread's real dev server (headless Chrome, 2x)."""
    frames = os.path.join(out, "browser")
    os.makedirs(frames, exist_ok=True)
    ground = Image.open(os.path.join(out, "backdrop.png")).convert("RGB")
    page = Image.open(page_path).convert("RGB")
    bw, bh = 1000, 700
    bx, by = (W - bw) // 2, (H - bh) // 2 + 4
    bar = 38
    view_h = bh - bar
    page = page.resize((bw, round(page.height * bw / page.width)), Image.LANCZOS)
    travel = min(330, page.height - view_h)
    title = sf(13, 600)
    n_frames = int(secs * 30)
    for n in range(n_frames):
        k = n / max(1, n_frames - 1)
        ease = k * k * (3 - 2 * k)
        top = round(travel * ease)
        img = ground.copy()
        sh = Image.new("L", (W, H), 0)
        ImageDraw.Draw(sh).rounded_rectangle((bx + 8, by + 18, bx + bw - 8, by + bh + 8), 14, fill=80)
        sh = sh.filter(ImageFilter.GaussianBlur(18))
        img = Image.composite(Image.new("RGB", (W, H), PLUM), img, sh)
        win = Image.new("RGB", (bw, bh), (0xF3, 0xEE, 0xF5))
        win.paste(page.crop((0, top, bw, top + view_h)), (0, bar))
        d = ImageDraw.Draw(win)
        d.rectangle((0, 0, bw, bar - 1), fill=(0xEC, 0xE6, 0xEF))
        d.line((0, bar - 1, bw, bar - 1), fill=(0xD8, 0xCF, 0xDC))
        for i, c in enumerate([CORAL, AMBER, (0x8F, 0xC9, 0x7E)]):
            d.ellipse((16 + i * 20, 13, 28 + i * 20, 25), fill=c)
        tab = "Retry visualizer"
        tw = d.textlength(tab, font=title)
        d.rounded_rectangle((90, 6, 90 + tw + 36, bar - 2), 7, fill=(0xFA, 0xF7, 0xFB))
        d.text((108, 11), tab, font=title, fill=(0x3A, 0x2C, 0x40))
        mask = Image.new("L", (bw, bh), 0)
        ImageDraw.Draw(mask).rounded_rectangle((0, 0, bw - 1, bh - 1), 12, fill=255)
        img.paste(win, (bx, by), mask)
        img.save(os.path.join(frames, f"{n:04}.png"))
    print(f"browser: {secs:.1f}s")


def one_more(out, secs=2.9):
    """`onemore/NNNN.png` (30 fps): "One more thing." on plum, fading in, for the iPhone's reveal."""
    frames = os.path.join(out, "onemore")
    os.makedirs(frames, exist_ok=True)
    f = sf(58, 600)
    text = "One more thing."
    probe = ImageDraw.Draw(Image.new("RGB", (W, H)))
    tw = probe.textlength(text, font=f)
    for n in range(int(secs * 30)):
        now = n / 30
        k = min(1.0, max(0.0, (now - 0.12) / 0.45))
        a = k * k * (3 - 2 * k)
        img = Image.new("RGB", (W, H), PLUM)
        d = ImageDraw.Draw(img)
        colour = tuple(round(p + (255 - p) * a) for p in PLUM)
        d.text(((W - tw) / 2, H / 2 - 40), text, font=f, fill=colour)
        img.save(os.path.join(frames, f"{n:04}.png"))
    print(f"one more thing: {secs:.1f}s")


if __name__ == "__main__":
    if sys.argv[1] == "browser":
        browser(sys.argv[2], sys.argv[3])
        sys.exit()
    if sys.argv[1] == "mcp":
        mcp_session(*sys.argv[2:6])
        sys.exit()
    out, icon, text = sys.argv[1:4]
    os.makedirs(out, exist_ok=True)
    backdrop(out)
    phone_parts(out)
    card(out, icon)
    one_more(out)
    terminal(out, text)
    print(f"phone screen {SCREEN_W}x{SCREEN_H}, bezel {BEZEL}, radius {RADIUS}")
