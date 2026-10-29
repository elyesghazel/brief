"""JetBrains Mono glyphs as SVG path data, so no logo file needs the font installed.

The font is the official JetBrains Mono release (OFL), pinned by version and hash and cached
in ~/.cache/brief-logo. Set BRIEF_LOGO_FONT to a JetBrainsMono[wght].ttf to skip the download.
"""
import hashlib
import io
import os
import urllib.request
import zipfile
from functools import lru_cache
from pathlib import Path

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

VERSION = "2.304"
URL = f"https://github.com/JetBrains/JetBrainsMono/releases/download/v{VERSION}/JetBrainsMono-{VERSION}.zip"
SHA256 = "6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf"
MEMBER = "fonts/variable/JetBrainsMono[wght].ttf"
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "brief-logo"


def font_file() -> Path:
    if env := os.environ.get("BRIEF_LOGO_FONT"):
        return Path(env)
    ttf = CACHE / f"JetBrainsMono-{VERSION}[wght].ttf"
    if not ttf.exists():
        req = urllib.request.Request(URL, headers={"User-Agent": "brief-logo-build"})
        data = urllib.request.urlopen(req).read()
        if hashlib.sha256(data).hexdigest() != SHA256:
            raise SystemExit(f"hash mismatch for {URL}")
        CACHE.mkdir(parents=True, exist_ok=True)
        ttf.write_bytes(zipfile.ZipFile(io.BytesIO(data)).read(MEMBER))
    return ttf


@lru_cache
def font(weight: int) -> TTFont:
    return instantiateVariableFont(TTFont(font_file()), {"wght": weight})


def text_path(text: str, weight: int, x: float, baseline: float, size: float,
              tracking: float = 0.0) -> tuple[str, float]:
    """Path data for `text` with its baseline at (x, baseline) at font size `size`. Returns (d, advance)."""
    f = font(weight)
    gs, cmap = f.getGlyphSet(), f.getBestCmap()
    scale = size / f["head"].unitsPerEm
    pen = SVGPathPen(gs, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
    cx = x
    for ch in text:
        g = gs[cmap[ord(ch)]]
        g.draw(TransformPen(pen, (scale, 0, 0, -scale, cx, baseline)))
        cx += g.width * scale + tracking * size
    return pen.getCommands(), cx - x


def bounds(text: str, weight: int):
    """Ink bounds (xMin, yMin, xMax, yMax) of `text` in font units, origin at the first glyph."""
    f = font(weight)
    gs, cmap = f.getGlyphSet(), f.getBestCmap()
    bp = BoundsPen(gs)
    cx = 0
    for ch in text:
        g = gs[cmap[ord(ch)]]
        g.draw(TransformPen(bp, (1, 0, 0, 1, cx, 0)))
        cx += g.width
    return bp.bounds
