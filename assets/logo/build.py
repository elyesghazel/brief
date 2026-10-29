"""Build the brief logo and icon set.

    uv run --no-project --with fonttools --with resvg-py --with pillow python -B assets/logo/build.py

The mark is a geometric lowercase b whose bowl is a letter (Brief = letter): a stem, a square
bowl, and the envelope flap cut into it. It is drawn on a 16 unit grid so 16 and 32 px land on
whole pixels. Small sizes (up to 32 px) fill the flap solid, a thin chevron would turn to grey there.
The wordmark text is JetBrains Mono, converted to outlines (see glyphs.py).
Every file under assets/logo and assets/icons is generated: change the numbers here and rebuild.
"""
import io
import shutil
from pathlib import Path

import resvg_py
from PIL import Image

from glyphs import bounds, font, text_path

ROOT = Path(__file__).resolve().parents[2]
LOGO = ROOT / "assets/logo"
ICONS = ROOT / "assets/icons/hicolor"
APP_ID = "ch.elyesghazel.brief"

BG, INK = "#000000", "#ffffff"
INK_LIGHT = "#0a0a0a"

GRID = 16
# stem from the ascender to the baseline, bowl from the x-height down, all in grid units
L, R, TOP, BOT, BOWL, T = 3, 13, 2, 14, 5, 2
RADIUS = 0.03        # tile corner, share of the side: nearly square
SIZES = (16, 24, 32, 48, 64, 128, 256, 512)
SMALL = 32           # up to here: solid flap


def mark_d(x0: float, top: float, x1: float, bot: float, bowl: float, t: float,
           small: bool, snap: float = 0) -> str:
    """Path data of the mark (evenodd): ink box x0..x1, top..bot, bowl from y=bowl down, walls t.

    `snap` rounds every point to that step, so small renders land on whole pixels."""
    q = (lambda v: round(v / snap) * snap) if snap else (lambda v: v)
    p = lambda x, y: f"{q(x):.2f} {q(y):.2f}"
    ix0, ix1, iy, ib = x0 + t, x1 - t, bowl + t, bot - t
    mid = (ix0 + ix1) / 2
    d = f"M{p(x0, top)}L{p(x0 + t, top)}L{p(x0 + t, bowl)}L{p(x1, bowl)}L{p(x1, bot)}L{p(x0, bot)}Z"
    if small:
        # the flap as a solid wedge hanging from the top of the bowl
        dip = iy + (ib - iy) * 0.85
        d += f"M{p(ix0, iy)}L{mid:.2f} {dip:.2f}L{p(ix1, iy)}L{p(ix1, ib)}L{p(ix0, ib)}Z"
    else:
        # the flap as a chevron, a little lighter than the walls
        band, dip = t * 0.62, iy + (ib - iy) * 0.72
        d += f"M{ix0:.2f} {iy:.2f}L{ix1:.2f} {iy:.2f}L{mid:.2f} {dip - band:.2f}Z"
        d += f"M{ix0:.2f} {iy + band:.2f}L{mid:.2f} {dip:.2f}L{ix1:.2f} {iy + band:.2f}L{p(ix1, ib)}L{p(ix0, ib)}Z"
    return d


def glyph_d(small: bool, size: float = 512, px: int = 0) -> str:
    """The mark on its 16 unit grid in a size x size box, snapped to `px` pixels if given."""
    u = size / GRID
    return mark_d(L * u, TOP * u, R * u, BOT * u, BOWL * u, T * u, small, size / px if px else 0)


def icon(px: int = 512, bg: str = BG, ink: str = INK) -> str:
    """The app icon: the mark on a nearly square tile. `px` picks the small or large drawing."""
    s = 512
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {s} {s}" width="{s}" height="{s}">'
            f'<rect width="{s}" height="{s}" rx="{s * RADIUS:g}" fill="{bg}"/>'
            f'<path fill="{ink}" fill-rule="evenodd" d="{glyph_d(px <= SMALL, px=px if px <= SMALL else 0)}"/></svg>')


def symbolic() -> str:
    """GTK symbolic icon: the bare mark on the 16 px grid, recoloured by the theme."""
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16">'
            f'<path fill="#000000" fill-rule="evenodd" d="{glyph_d(True, 16, 16)}"/></svg>')


def wordmark(ink: str, size: float = 96, weight: int = 700) -> str:
    """`brief` in JetBrains Mono with the mark standing in for the b: stem to the ascender,
    bowl to the x-height, walls matched to the text weight."""
    f = font(weight)
    sc = size / f["head"].unitsPerEm
    bx0, _, bx1, asc = bounds("b", weight)
    xh = f["OS/2"].sxHeight
    asc_px = asc * sc
    baseline = asc_px
    t = asc_px * T / (BOT - TOP) * 0.82   # the text's stems are a little lighter than the icon's walls
    widen = 0.03 * size                     # the square bowl needs a touch more room than a round one
    mark = mark_d(bx0 * sc - widen, 0, bx1 * sc + widen, baseline, baseline - xh * sc, t, False)
    adv = f["hmtx"]["b"][0] * sc
    d, _ = text_path("rief", weight, adv, baseline, size)
    rx1 = bounds("brief", weight)[2] * sc
    w, h = rx1, baseline - f["OS/2"].sTypoDescender * sc
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:.1f} {h:.1f}" width="{w:.0f}" height="{h:.0f}">'
            f'<path fill="{ink}" fill-rule="evenodd" d="{mark}"/><path fill="{ink}" d="{d}"/></svg>')


def png(svg: str, w: int, h: int | None = None) -> Image.Image:
    data = bytes(resvg_py.svg_to_bytes(svg_string=svg, width=w, height=h or w))
    return Image.open(io.BytesIO(data)).convert("RGBA")


def write(path: Path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, Image.Image):
        data.save(path, optimize=True)
    else:
        path.write_text(data)


def main():
    shutil.rmtree(ICONS, ignore_errors=True)
    write(LOGO / "brief.svg", icon())
    write(LOGO / "brief-small.svg", icon(16))
    write(LOGO / "brief-symbolic.svg", symbolic())
    write(LOGO / "wordmark-dark.svg", wordmark(INK))
    write(LOGO / "wordmark-light.svg", wordmark(INK_LIGHT))

    write(ICONS / f"scalable/apps/{APP_ID}.svg", icon())
    write(ICONS / f"symbolic/apps/{APP_ID}-symbolic.svg", symbolic())
    for px in SIZES:
        write(ICONS / f"{px}x{px}/apps/{APP_ID}.png", png(icon(px), px))

    ico = [png(icon(px), px) for px in (16, 24, 32, 48, 64, 128, 256)]
    ico[-1].save(LOGO / "brief.ico", sizes=[i.size for i in ico], append_images=ico[:-1])
    print("built", len(list(ICONS.rglob("*.*"))), "icons")


if __name__ == "__main__":
    main()
