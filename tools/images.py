#!/usr/bin/env python3
"""Draw XivLantern's icon and banner: a warm paper lantern glowing in the dark.

Original art, drawn from shapes here; no game art. Writes images/icon.svg and
images/banner.svg, and, when rsvg-convert is installed, the PNGs next to them
(icon 512x512, banner 730x380, the sizes the other mods use).

  tools/images.py
"""
from __future__ import annotations

import math
import random
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "images"

DEFS = """
  <radialGradient id="paper" cx="50%" cy="46%" r="60%">
    <stop offset="0" stop-color="#fff6d8"/>
    <stop offset="0.35" stop-color="#ffd27a"/>
    <stop offset="0.75" stop-color="#f08a36"/>
    <stop offset="1" stop-color="#b8461c"/>
  </radialGradient>
  <radialGradient id="glow" cx="50%" cy="50%" r="50%">
    <stop offset="0" stop-color="#ffcf73" stop-opacity="0.60"/>
    <stop offset="0.45" stop-color="#ff9f43" stop-opacity="0.22"/>
    <stop offset="1" stop-color="#ff7a1a" stop-opacity="0"/>
  </radialGradient>
  <linearGradient id="cap" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="#4a2e1b"/>
    <stop offset="1" stop-color="#24150c"/>
  </linearGradient>
  <filter id="soft" x="-50%" y="-50%" width="200%" height="200%">
    <feGaussianBlur stdDeviation="6"/>
  </filter>
"""

TOP, BOTTOM = 58.0, 250.0


def half_width(y: float) -> float:
    """Half the lantern body's width at height y (local units, body 58..250)."""
    t = (y - TOP) / (BOTTOM - TOP)
    return 38.0 + 52.0 * math.sin(math.pi * t) ** 0.8


def lantern(x: float, y: float, scale: float, glow: bool = True, opacity: float = 1.0) -> str:
    """One lantern whose local box (200 x 300, hanging point at (100, 0)) is placed at (x, y) and scaled."""
    parts: list[str] = []
    if glow:
        parts.append('<circle cx="100" cy="155" r="170" fill="url(#glow)"/>')
    parts.append('<line x1="100" y1="-40" x2="100" y2="42" stroke="#6b4a2b" stroke-width="3" stroke-linecap="round"/>')
    # the body: sampled outline so the ribs line up with it
    left = [(100 - half_width(yy), yy) for yy in [TOP + i * (BOTTOM - TOP) / 40 for i in range(41)]]
    right = [(100 + half_width(yy), yy) for yy in reversed([p[1] for p in left])]
    pts = " ".join(f"{px:.1f},{py:.1f}" for px, py in left + right)
    parts.append(f'<polygon points="{pts}" fill="url(#paper)"/>')
    # the light inside
    parts.append('<ellipse cx="100" cy="160" rx="34" ry="52" fill="#fff8e6" opacity="0.55" filter="url(#soft)"/>')
    # horizontal ribs of the paper
    for i in range(1, 8):
        yy = TOP + i * (BOTTOM - TOP) / 8
        hw = half_width(yy)
        parts.append(
            f'<path d="M {100 - hw:.1f} {yy:.1f} Q 100 {yy + 7:.1f} {100 + hw:.1f} {yy:.1f}" '
            'fill="none" stroke="#c4561d" stroke-opacity="0.45" stroke-width="2"/>'
        )
    # vertical seams
    for k in (-0.62, -0.28, 0.28, 0.62):
        seam = [(100 + k * half_width(yy), yy) for yy in [TOP + i * (BOTTOM - TOP) / 20 for i in range(21)]]
        d = "M " + " L ".join(f"{px:.1f} {py:.1f}" for px, py in seam)
        parts.append(f'<path d="{d}" fill="none" stroke="#b8461c" stroke-opacity="0.35" stroke-width="1.6"/>')
    # caps, with a gold rim
    parts.append('<rect x="60" y="40" width="80" height="20" rx="6" fill="url(#cap)" stroke="#d9a441" stroke-width="2"/>')
    parts.append('<rect x="64" y="246" width="72" height="16" rx="5" fill="url(#cap)" stroke="#d9a441" stroke-width="2"/>')
    # knot and tassel
    parts.append('<circle cx="100" cy="270" r="5" fill="#d9a441"/>')
    for dx in (-6, -3, 0, 3, 6):
        parts.append(
            f'<line x1="100" y1="274" x2="{100 + dx * 1.6:.1f}" y2="312" stroke="#b3261e" stroke-width="2.2" stroke-linecap="round"/>'
        )
    body = "\n    ".join(parts)
    return f'<g transform="translate({x:.1f} {y:.1f}) scale({scale:.3f}) translate(-100 0)" opacity="{opacity}">\n    {body}\n  </g>'


def icon() -> str:
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
<defs>{DEFS}
  <radialGradient id="ink" cx="50%" cy="40%" r="75%">
    <stop offset="0" stop-color="#2a1d24"/>
    <stop offset="1" stop-color="#08090d"/>
  </radialGradient>
</defs>
<rect width="512" height="512" rx="96" fill="url(#ink)"/>
  {lantern(256, 78, 1.18)}
</svg>
"""


def banner() -> str:
    rng = random.Random(7)  # the same fireflies every time
    dots = []
    for _ in range(38):
        cx, cy = rng.uniform(0, 730), rng.uniform(0, 380)
        r = rng.uniform(0.8, 2.2)
        dots.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="{r:.2f}" fill="#ffcf73" opacity="{rng.uniform(0.15, 0.6):.2f}"/>')
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="730" height="380" viewBox="0 0 730 380">
<defs>{DEFS}
  <linearGradient id="night" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#1c1420"/>
    <stop offset="0.6" stop-color="#0d0e14"/>
    <stop offset="1" stop-color="#07080b"/>
  </linearGradient>
</defs>
<rect width="730" height="380" fill="url(#night)"/>
{chr(10).join(dots)}
  {lantern(650, -18, 0.30, opacity=0.40)}
  {lantern(560, 292, 0.20, opacity=0.30)}
  {lantern(170, 30, 1.02)}
<text x="330" y="176" font-family="Noto Serif, serif" font-size="58" font-weight="700" fill="#f5c76b">XivLantern</text>
<text x="332" y="216" font-family="Noto Sans, sans-serif" font-size="22" fill="#e8dcc6">Light a lantern for your friends.</text>
<text x="332" y="248" font-family="Noto Sans, sans-serif" font-size="15" fill="#9d95a3">Friends, chat and channels, peer to peer.</text>
</svg>
"""


def main() -> int:
    OUT.mkdir(exist_ok=True)
    for name, svg, (w, h) in (("icon", icon(), (512, 512)), ("banner", banner(), (730, 380))):
        path = OUT / f"{name}.svg"
        path.write_text(svg, encoding="utf-8")
        print(f"wrote {path.relative_to(ROOT)}")
        if shutil.which("rsvg-convert"):
            png = OUT / f"{name}.png"
            subprocess.run(["rsvg-convert", "-w", str(w), "-h", str(h), "-o", str(png), str(path)], check=True)
            print(f"wrote {png.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
