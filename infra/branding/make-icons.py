"""Generates the Aionix Connect icons (Windows .ico + PNGs for the website).

    python -I infra/branding/make-icons.py

Writes branding/icons/setup.ico (app, installer) and branding/icons/setup-512.png. The
mark is the same "A" monogram the app draws at runtime (crates/desktop/src/icon.rs), on a
diagonal blue→violet gradient: one app for both sides of a support session.
Rendered at 1024 px and downsampled per size so small icons stay crisp.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "branding" / "icons"

VARIANTS = {
    # name: (gradient start, gradient end)
    "setup": ((0x5E, 0x8C, 0xFF), (0x7A, 0x45, 0xF0)),
}
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
BIG = 1024


def gradient(size, a, b):
    """Diagonal gradient from top-left (a) to bottom-right (b)."""
    img = Image.new("RGB", (size, size))
    px = img.load()
    for y in range(size):
        for x in range(size):
            t = (x + y) / (2 * (size - 1))
            px[x, y] = tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))
    return img


def mark(a, b):
    s = BIG
    radius = int(s * 0.235)
    base = gradient(s, a, b).convert("RGBA")
    # Soft radial light from the top-left corner for depth.
    glow = Image.new("L", (s, s), 0)
    ImageDraw.Draw(glow).ellipse((-s * 0.45, -s * 0.55, s * 0.75, s * 0.55), fill=255)
    glow = glow.filter(ImageFilter.GaussianBlur(s * 0.12)).point(lambda v: int(v * 0.22))
    light = Image.new("RGBA", (s, s), (255, 255, 255, 0))
    light.putalpha(glow)
    base = Image.alpha_composite(base, light)
    white = Image.new("RGBA", (s, s), (255, 255, 255, 255))
    # Monogram: same unit coordinates as icon.rs, rounded strokes.
    glyph = Image.new("L", (s, s), 0)
    d = ImageDraw.Draw(glyph)
    w = int(s * 0.10)
    pts = [((0.27, 0.76), (0.50, 0.24)), ((0.50, 0.24), (0.73, 0.76)), ((0.37, 0.58), (0.63, 0.58))]
    for (x0, y0), (x1, y1) in pts:
        p0, p1 = (x0 * s, y0 * s), (x1 * s, y1 * s)
        d.line([p0, p1], fill=255, width=w)
        for p in (p0, p1):
            d.ellipse((p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2), fill=255)
    base = Image.composite(white, base, glyph)
    # Rounded-square mask.
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, s - 1, s - 1), radius=radius, fill=255)
    out = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    out.paste(base, (0, 0), mask)
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, (a, b) in VARIANTS.items():
        big = mark(a, b)
        big.resize((512, 512), Image.LANCZOS).save(OUT / f"{name}-512.png")
        frames = [big.resize((n, n), Image.LANCZOS) for n in SIZES]
        frames[-1].save(OUT / f"{name}.ico", format="ICO", sizes=[(n, n) for n in SIZES],
                        append_images=frames[:-1])
        print(f"{name}: {OUT / (name + '.ico')}")


if __name__ == "__main__":
    main()
