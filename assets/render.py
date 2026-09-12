"""Render recorded frames to a GIF."""
import os, pickle, shutil, subprocess, sys, tempfile
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FRAMES = os.environ.get("DEMO_FRAMES", os.path.join(HERE, "frames.pkl"))
OUT = os.environ.get("DEMO_GIF", os.path.join(HERE, "demo.gif"))
FPS = 10
MAX_HOLD = 7          # frames a static screen may occupy, so waits compress
SIZE = 13
PAD = 14

# Solarized Light, the theme the recorded session runs.
BG = (253, 246, 227)
FG = (101, 123, 131)
NAMED = {
    "black": (7, 54, 66), "red": (220, 50, 47), "green": (133, 153, 0),
    "brown": (181, 137, 0), "yellow": (181, 137, 0), "blue": (38, 139, 210),
    "magenta": (211, 54, 130), "cyan": (42, 161, 152), "white": (238, 232, 213),
    "default": FG,
}


def colour(name, fallback):
    if name in NAMED:
        return NAMED[name]
    if isinstance(name, str) and len(name) == 6:
        try:
            return tuple(int(name[i:i + 2], 16) for i in (0, 2, 4))
        except ValueError:
            pass
    return fallback


def font():
    for family in ("JetBrainsMono Nerd Font", "JetBrains Mono", "DejaVu Sans Mono", "monospace"):
        found = subprocess.run(["fc-match", "-f", "%{file}", family],
                               capture_output=True, text=True)
        if found.returncode == 0 and found.stdout.strip():
            return ImageFont.truetype(found.stdout.strip(), SIZE)
    sys.exit("no font")


def glyph_fonts(base):
    """Pillow does no font fallback, so resolve a face per character."""
    blank = Image.new("L", (40, 40))
    def bitmap(f, ch):
        im = blank.copy()
        ImageDraw.Draw(im).text((4, 4), ch, font=f, fill=255)
        return im.tobytes()
    missing = bitmap(base, "\uffff")
    cache = {}

    def pick(ch):
        if ch not in cache:
            face = base
            if bitmap(base, ch) == missing:
                found = subprocess.run(
                    ["fc-match", "-f", "%{file}", f":charset={ord(ch):x}"],
                    capture_output=True, text=True).stdout.strip()
                if found:
                    face = ImageFont.truetype(found, SIZE)
            cache[ch] = face
        return cache[ch]

    return pick


def select(frames):
    out, run, prev = [], 0, None
    for _, rows in frames:
        key = tuple("".join(c[0] for c in r) for r in rows)
        if key == prev:
            run += 1
            if run > MAX_HOLD:
                continue
        else:
            run = 0
        prev = key
        out.append(rows)
    return out


def main():
    frames = pickle.load(open(FRAMES, "rb"))
    picked = select(frames)
    f = font()
    pick = glyph_fonts(f)
    adv = f.getlength("M")
    rows_n, cols_n = len(picked[0]), len(picked[0][0])
    line_h = SIZE + 5
    W = int(adv * cols_n) + PAD * 2
    H = line_h * rows_n + PAD * 2
    print(f"{len(frames)} recorded -> {len(picked)} frames, {W}x{H}")
    with tempfile.TemporaryDirectory() as tmp:
        for i, rows in enumerate(picked):
            img = Image.new("RGB", (W, H), BG)
            d = ImageDraw.Draw(img)
            for y, row in enumerate(rows):
                for x, (ch, fg, bg, bold, reverse) in enumerate(row):
                    if ch == " " and bg in ("default", None):
                        continue
                    px, py = PAD + x * adv, PAD + y * line_h
                    f_col = colour(fg, FG)
                    b_col = BG if bg == "default" else colour(bg, BG)
                    if reverse:
                        f_col, b_col = b_col, f_col
                    if b_col != BG:
                        d.rectangle([px, py, px + adv, py + line_h], fill=b_col)
                    if ch != " ":
                        d.text((px, py), ch, font=pick(ch), fill=f_col)
            img.save(f"{tmp}/f{i:04d}.png")
        pal = f"{tmp}/pal.png"
        run = lambda *a: subprocess.run(a, check=True, capture_output=True)
        run("ffmpeg", "-y", "-i", f"{tmp}/f%04d.png",
            "-vf", "palettegen=max_colors=128:stats_mode=full", pal)
        out = OUT
        run("ffmpeg", "-y", "-framerate", str(FPS), "-i", f"{tmp}/f%04d.png", "-i", pal,
            "-lavfi", "paletteuse=dither=none", "-loop", "0", out)
        print(out, os.path.getsize(out), "bytes")


if __name__ == "__main__":
    main()
