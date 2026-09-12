"""Render recorded frames to a GIF."""
import os, pickle, shutil, subprocess, sys, tempfile
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
FRAMES = os.environ.get("DEMO_FRAMES", "/tmp/herdr-dictate-demo-frames.pkl")
OUT = os.environ.get("DEMO_GIF", os.path.join(REPO, "assets", "demo.gif"))
CARD = os.environ.get("DEMO_CARD", os.path.join(REPO, ".github", "social-preview.png"))
FPS = 10
MAX_HOLD = 7          # frames a static screen may occupy, so waits compress
SIZE = int(os.environ.get("DEMO_FONT_SIZE", 26))   # 2x, so HiDPI screens get real pixels
SCALE = int(os.environ.get("DEMO_SCALE", 2))   # supersample, then average down
COLOURS = int(os.environ.get("DEMO_COLOURS", 256))
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
    """Rendered at SCALE and averaged down, so glyph edges come out smooth."""
    for family in ("JetBrainsMono Nerd Font", "JetBrains Mono", "DejaVu Sans Mono", "monospace"):
        found = subprocess.run(["fc-match", "-f", "%{file}", family],
                               capture_output=True, text=True)
        if found.returncode == 0 and found.stdout.strip():
            return ImageFont.truetype(found.stdout.strip(), SIZE * SCALE)
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
                    face = ImageFont.truetype(found, SIZE * SCALE)
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


def card(frame):
    """GitHub renders a social preview at 1280x640; anything else is cropped."""
    shot = Image.open(frame).convert("RGB")
    w, h = shot.size
    out = Image.new("RGB", (1280, 640), BG)
    shot = shot.resize((1280, round(h * 1280 / w)), Image.LANCZOS)
    out.paste(shot, (0, max(0, (640 - shot.size[1]) // 2)))
    out.save(CARD)
    print(CARD)


def main():
    frames = pickle.load(open(FRAMES, "rb"))
    picked = select(frames)
    f = font()
    pick = glyph_fonts(f)
    adv = f.getlength("M")
    rows_n, cols_n = len(picked[0]), len(picked[0][0])
    line_h = SIZE * SCALE + 5 * SCALE
    W = int(adv * cols_n) + PAD * 2 * SCALE
    H = line_h * rows_n + PAD * 2 * SCALE
    print(f"{len(frames)} recorded -> {len(picked)} frames, {W}x{H}")
    with tempfile.TemporaryDirectory() as tmp:
        for i, rows in enumerate(picked):
            img = Image.new("RGB", (W, H), BG)
            d = ImageDraw.Draw(img)
            for y, row in enumerate(rows):
                for x, (ch, fg, bg, bold, reverse) in enumerate(row):
                    if ch == " " and bg in ("default", None):
                        continue
                    px, py = PAD * SCALE + x * adv, PAD * SCALE + y * line_h
                    f_col = colour(fg, FG)
                    b_col = BG if bg == "default" else colour(bg, BG)
                    if reverse:
                        f_col, b_col = b_col, f_col
                    if b_col != BG:
                        d.rectangle([px, py, px + adv, py + line_h], fill=b_col)
                    if ch != " ":
                        d.text((px, py), ch, font=pick(ch), fill=f_col)
            img = img.resize((W // SCALE, H // SCALE), Image.LANCZOS)
            img.save(f"{tmp}/f{i:04d}.png")
        pal = f"{tmp}/pal.png"
        run = lambda *a: subprocess.run(a, check=True, capture_output=True)
        run("ffmpeg", "-y", "-i", f"{tmp}/f%04d.png",
            "-vf", f"palettegen=max_colors={COLOURS}:stats_mode=full", pal)
        card(f"{tmp}/f{int(len(picked) * 0.65):04d}.png")
        out = OUT
        run("ffmpeg", "-y", "-framerate", str(FPS), "-i", f"{tmp}/f%04d.png", "-i", pal,
            "-lavfi", "paletteuse=dither=none", "-loop", "0", out)
        print(out, os.path.getsize(out), "bytes")


if __name__ == "__main__":
    main()
