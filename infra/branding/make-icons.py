"""Generates the Aionix Connect icons (Windows .ico + PNGs).

    python -I infra/branding/make-icons.py

Writes branding/icons/setup.ico (app, installer, shortcuts), branding/icons/setup-512.png,
branding/icons/app-256.png (window icon) and the website's site/public/assets/*.png. The mark: two landscape screens that
overlap (the front one on a stand), the region they share filled solid; it is what both
people see during a session. It sits on a flat blue rounded square. The same geometry
(24-unit grid) is drawn at runtime by `theme::paint_mark` (crates/desktop/src/theme.rs)
and by the consent prompt (crates/platform/src/windows/consent.rs).
Rendered at 1024 px and downsampled per size; small sizes use a heavier stroke so the
two outlines never melt into one shape.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "branding" / "icons"

BIG = 1024
SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
TILE = (0x1B, 0x6A, 0xEB)
CORNER = 0.225  # tile corner radius, fraction of the side
# Mark geometry on a 24-unit grid (kept in sync with theme.rs).
SCREEN_A = (3.0, 3.6, 15.6, 11.8)
SCREEN_B = (8.4, 8.8, 21.0, 17.0)
RADIUS = 1.6
# Stand of the front screen: neck and base.
STAND = [((13.85, 17.0, 15.55, 19.4), 0.4), ((11.6, 19.0, 17.8, 20.6), 0.6)]


def u(v):
    return round(v * BIG / 24)


def rounded(box, r):
    m = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(m).rounded_rectangle([u(box[0]), u(box[1]), u(box[2]), u(box[3])],
                                        radius=u(r), fill=255)
    return m


def outline(box, r, w):
    inner = (box[0] + w, box[1] + w, box[2] - w, box[3] - w)
    return ImageChops.subtract(rounded(box, r), rounded(inner, max(r - w, 0.3)))


def mark_mask(stroke):
    a, b = SCREEN_A, SCREEN_B
    shared = (max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3]))
    m = ImageChops.lighter(outline(a, RADIUS, stroke), outline(b, RADIUS, stroke))
    m = ImageChops.lighter(m, rounded(shared, RADIUS * 0.3))
    for box, r in STAND:
        m = ImageChops.lighter(m, rounded(box, r))
    return m


def tile():
    mask = Image.new("L", (BIG, BIG), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, BIG - 1, BIG - 1),
                                           radius=int(BIG * CORNER), fill=255)
    out = Image.new("RGBA", (BIG, BIG), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (BIG, BIG), TILE + (255,)), (0, 0), mask)
    return out


def icon(stroke):
    img = tile()
    white = Image.new("RGBA", (BIG, BIG), (255, 255, 255, 255))
    img.paste(white, (0, 0), ImageChops.multiply(mark_mask(stroke), img.split()[3]))
    return img


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    regular, heavy = icon(1.7), icon(2.1)
    regular.resize((512, 512), Image.LANCZOS).save(OUT / "setup-512.png")
    regular.resize((256, 256), Image.LANCZOS).save(OUT / "app-256.png")
    frames = [(heavy if n <= 32 else regular).resize((n, n), Image.LANCZOS) for n in SIZES]
    frames[-1].save(OUT / "setup.ico", format="ICO", sizes=[(n, n) for n in SIZES],
                    append_images=frames[:-1])
    # Website (site/public/assets): page icon, favicon, Apple touch icon, replica icon.
    site = ROOT / "site" / "public" / "assets"
    site.mkdir(parents=True, exist_ok=True)
    regular.resize((512, 512), Image.LANCZOS).save(site / "setup-512.png")
    regular.resize((96, 96), Image.LANCZOS).save(site / "app-96.png")
    heavy.resize((32, 32), Image.LANCZOS).save(site / "favicon-32.png")
    touch = Image.new("RGBA", (180, 180), TILE + (255,))
    mark_only = icon(1.7).resize((180, 180), Image.LANCZOS)
    touch.alpha_composite(mark_only)
    touch.convert("RGB").save(site / "apple-touch-icon.png")
    # Contact sheet for checking the small sizes by eye (not shipped).
    sheet = Image.new("RGBA", (40 + 64 * len(SIZES[:6]), 2 * 80), (0xF3, 0xF3, 0xF5, 255))
    ImageDraw.Draw(sheet).rectangle([0, 80, sheet.width, 160], fill=(0x1C, 0x1C, 0x1F, 255))
    for j, f in enumerate(frames[:6]):
        for row in (0, 1):
            sheet.alpha_composite(f, (20 + j * 64, 16 + row * 80))
    sheet.save(ROOT / "target" / "icon-sizes.png")
    print(f"icons: {OUT}")


if __name__ == "__main__":
    main()
