#!/usr/bin/env python3
"""Draw XivLantern's icon, banner and README heroes: a warm paper lantern under the aetheryte.

Original art, drawn from shapes here; no game art. It uses the house style of the other
mods (night-navy sky and sea, the crystal with its gold ring, gold frame and serif title)
and writes images/icon.svg (512x512), images/banner.svg (730x380) and
images/readme/hero-dark.svg / hero-light.svg (1280x640), and, when rsvg-convert is
installed, the PNG next to each.

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

TITLE = "XivLantern"
TAGLINE = "Light a lantern for your friends."
SUBLINE = "friends · 1:1 chat · channels · peer to peer"

SERIF = "'Noto Serif','STIX Two Text',Georgia,serif"
MONO = "'Fira Code','Source Code Pro','DejaVu Sans Mono',monospace"

# --- the house palette, dark and light -----------------------------------------------------

DARK = {
    "sky": ("#050a1c", "#0e1d45", "#1d3a72"),
    "sea": ("#10284f", "#040915"),
    "horizon": ("#5fb8ff", "0.4"),
    "gold": ("#fbeab5", "#d6a854", "#94682c"),
    "stars": 'fill="#d8ecff"',
    "shore": 'stroke="#6fb9f0" stroke-opacity="0.35"',
    "ripples": 'stroke="#7fc8ff" opacity="0.25"',
    "tagline": "#f1dfae",
    "subline": 'fill="#8ff0ff" opacity="0.85" filter="url(#glow)"',
}
LIGHT = {
    "sky": ("#e9f0fb", "#f7eedb", "#f5d9ac"),
    "sea": ("#c9dcf2", "#8fb0dc"),
    "horizon": ("#ffcf7a", "0.75"),
    "gold": ("#c99a4b", "#94682c", "#5a3d14"),
    "stars": 'fill="#c99a4b" opacity="0.55"',
    "shore": 'stroke="#5f8fc8" stroke-opacity="0.6"',
    "ripples": 'stroke="#ffffff" opacity="0.7"',
    "tagline": "#3d2f14",
    "subline": 'fill="#17577d"',
}


def defs(p: dict) -> str:
    g = p["gold"]
    return f"""
    <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{p['sky'][0]}"/><stop offset="0.55" stop-color="{p['sky'][1]}"/><stop offset="1" stop-color="{p['sky'][2]}"/></linearGradient>
    <linearGradient id="sea" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{p['sea'][0]}"/><stop offset="1" stop-color="{p['sea'][1]}"/></linearGradient>
    <radialGradient id="horizon" cx="72%" cy="100%" r="60%"><stop offset="0" stop-color="{p['horizon'][0]}" stop-opacity="{p['horizon'][1]}"/><stop offset="1" stop-color="{p['horizon'][0]}" stop-opacity="0"/></radialGradient>
    <radialGradient id="halo" cx="50%" cy="50%" r="50%"><stop offset="0" stop-color="#8fe3ff" stop-opacity="0.55"/><stop offset="0.45" stop-color="#4fb0f0" stop-opacity="0.18"/><stop offset="1" stop-color="#4fb0f0" stop-opacity="0"/></radialGradient>
    <linearGradient id="gold" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{g[0]}"/><stop offset="0.5" stop-color="{g[1]}"/><stop offset="1" stop-color="{g[2]}"/></linearGradient>
    <linearGradient id="goldX" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#8a6128"/><stop offset="0.35" stop-color="#f6e2a4"/><stop offset="1" stop-color="#9a6e2e"/></linearGradient>
    <linearGradient id="goldH" gradientUnits="userSpaceOnUse" x1="84" y1="0" x2="520" y2="0"><stop offset="0" stop-color="#d6a854" stop-opacity="0"/><stop offset="0.15" stop-color="#d6a854"/><stop offset="0.85" stop-color="#d6a854"/><stop offset="1" stop-color="#d6a854" stop-opacity="0"/></linearGradient>
    <linearGradient id="fA" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#d4f7ff"/><stop offset="1" stop-color="#3a9be0"/></linearGradient>
    <linearGradient id="fB" x1="1" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#6cd0f8"/><stop offset="1" stop-color="#1a5aa0"/></linearGradient>
    <linearGradient id="glass" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1c3a70" stop-opacity="0.9"/><stop offset="1" stop-color="#071230" stop-opacity="0.9"/></linearGradient>
    <linearGradient id="glare" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffffff" stop-opacity="0.16"/><stop offset="1" stop-color="#ffffff" stop-opacity="0"/></linearGradient>
    <linearGradient id="refl" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffc861" stop-opacity="0.5"/><stop offset="1" stop-color="#ffc861" stop-opacity="0"/></linearGradient>
    <radialGradient id="paper" cx="50%" cy="46%" r="60%"><stop offset="0" stop-color="#fff6d8"/><stop offset="0.35" stop-color="#ffd27a"/><stop offset="0.75" stop-color="#f08a36"/><stop offset="1" stop-color="#b8461c"/></radialGradient>
    <radialGradient id="warm" cx="50%" cy="50%" r="50%"><stop offset="0" stop-color="#ffcf73" stop-opacity="0.6"/><stop offset="0.45" stop-color="#ff9f43" stop-opacity="0.2"/><stop offset="1" stop-color="#ff7a1a" stop-opacity="0"/></radialGradient>
    <filter id="glow" x="-40%" y="-40%" width="180%" height="180%"><feGaussianBlur stdDeviation="5" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
    <filter id="bigglow" x="-40%" y="-40%" width="180%" height="180%"><feGaussianBlur stdDeviation="10" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
    <filter id="blur"><feGaussianBlur stdDeviation="3"/></filter>
    <filter id="soft" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="6"/></filter>"""


def stars(seed: int, n: int, w: float, h: float) -> str:
    rng = random.Random(seed)  # the same sky every time
    return "\n    ".join(
        f'<circle cx="{rng.uniform(0, w):.0f}" cy="{rng.uniform(0, h):.0f}" r="{rng.choice((0.8, 1, 1.1, 1.3, 1.6, 2.1))}" '
        f'opacity="{rng.uniform(0.35, 1):.2f}"/>'
        for _ in range(n)
    )


def crystal(cx: float, cy: float, s: float) -> str:
    """The aetheryte crystal inside its gold ring: back arc, crystal, front arc."""
    return f"""<g transform="translate({cx} {cy}) scale({s})">
    <circle r="150" fill="url(#halo)"/>
    <path d="M-150 10 A150 27 0 0 1 150 10" fill="none" stroke="url(#gold)" stroke-width="8" opacity="0.9"/>
    <g filter="url(#bigglow)">
      <path d="M0 -84 L40 -8 L0 60 Z" fill="url(#fB)"/>
      <path d="M0 -84 L-40 -8 L0 60 Z" fill="url(#fA)"/>
    </g>
    <path d="M0 -84 L40 -8 L0 60 L-40 -8 Z M0 -84 L0 60" fill="none" stroke="#e8fbff" stroke-width="2.5" stroke-linejoin="round" opacity="0.85"/>
    <path d="M-150 10 A150 27 0 0 0 150 10" fill="none" stroke="url(#gold)" stroke-width="8"/>
  </g>"""


# --- the lantern ---------------------------------------------------------------------------

TOP, BOTTOM = 58.0, 250.0


def half_width(y: float) -> float:
    """Half the lantern body's width at height y (local units, body 58..250)."""
    t = (y - TOP) / (BOTTOM - TOP)
    return 38.0 + 52.0 * math.sin(math.pi * t) ** 0.8


def lantern(x: float, y: float, scale: float, string: bool = True, opacity: float = 1.0) -> str:
    """One lantern whose local box (200 x 312, hanging point at (100, 0)) is placed at (x, y) and scaled."""
    parts: list[str] = ['<circle cx="100" cy="155" r="175" fill="url(#warm)"/>']
    if string:
        parts.append('<line x1="100" y1="-40" x2="100" y2="42" stroke="#d6a854" stroke-width="3" stroke-linecap="round"/>')
    left = [(100 - half_width(yy), yy) for yy in [TOP + i * (BOTTOM - TOP) / 40 for i in range(41)]]
    right = [(100 + half_width(yy), yy) for yy in reversed([p[1] for p in left])]
    pts = " ".join(f"{px:.1f},{py:.1f}" for px, py in left + right)
    parts.append(f'<polygon points="{pts}" fill="url(#paper)" filter="url(#glow)"/>')
    parts.append('<ellipse cx="100" cy="160" rx="34" ry="52" fill="#fff8e6" opacity="0.55" filter="url(#soft)"/>')
    for i in range(1, 8):  # the paper's ribs
        yy = TOP + i * (BOTTOM - TOP) / 8
        hw = half_width(yy)
        parts.append(
            f'<path d="M {100 - hw:.1f} {yy:.1f} Q 100 {yy + 7:.1f} {100 + hw:.1f} {yy:.1f}" '
            'fill="none" stroke="#c4561d" stroke-opacity="0.45" stroke-width="2"/>'
        )
    for k in (-0.62, -0.28, 0.28, 0.62):  # its seams
        seam = [(100 + k * half_width(yy), yy) for yy in [TOP + i * (BOTTOM - TOP) / 20 for i in range(21)]]
        d = "M " + " L ".join(f"{px:.1f} {py:.1f}" for px, py in seam)
        parts.append(f'<path d="{d}" fill="none" stroke="#b8461c" stroke-opacity="0.35" stroke-width="1.6"/>')
    parts.append('<rect x="60" y="40" width="80" height="20" rx="6" fill="url(#goldX)" stroke="#fbe9b8" stroke-width="2.5"/>')
    parts.append('<rect x="64" y="246" width="72" height="16" rx="5" fill="url(#goldX)" stroke="#fbe9b8" stroke-width="2.5"/>')
    parts.append('<circle cx="100" cy="270" r="5" fill="#f6e2a4"/>')
    for dx in (-6, -3, 0, 3, 6):  # the tassel
        parts.append(
            f'<line x1="100" y1="274" x2="{100 + dx * 1.6:.1f}" y2="312" stroke="#d6a854" stroke-width="2.2" stroke-linecap="round"/>'
        )
    body = "\n    ".join(parts)
    return f'<g transform="translate({x:.1f} {y:.1f}) scale({scale:.3f}) translate(-100 0)" opacity="{opacity}">\n    {body}\n  </g>'


# --- floating glass cards ------------------------------------------------------------------


def card(x: float, y: float, rot: float, w: float, h: float, body: str, amber: bool = False) -> str:
    edge = "#ffc861" if amber else "#6fd6ff"
    m = w / 2
    return f"""<g transform="translate({x} {y}) rotate({rot})" filter="url(#glow)">
      <path d="M0 8 Q{m} -4 {w} 8 L{w} {h - 8} Q{m} {h + 4} 0 {h - 8} Z" fill="url(#glass)" stroke="{edge}" stroke-width="2.5"/>
      <path d="M6 14 Q{m} 3 {w - 6} 14 L{w - 6} 40 Q{m} 30 6 40 Z" fill="url(#glare)"/>
      {body}
    </g>"""


def cards() -> str:
    friends = (
        '<text x="14" y="36" fill="#8ff0ff">friends</text>'
        '<circle cx="20" cy="55" r="5" fill="#ffd27a"/><text x="32" y="60" fill="#cfe4ff">Aya · online</text>'
        '<circle cx="20" cy="77" r="5" fill="none" stroke="#9fb8e0" stroke-width="1.5"/><text x="32" y="82" fill="#9fb8e0">Kiri · away</text>'
    )
    chat = (
        '<text x="14" y="36" fill="#8ff0ff">1:1 chat</text>'
        '<text x="14" y="60" fill="#cfe4ff">on my way!</text>'
        '<text x="14" y="82" fill="#9fb8e0">delivered</text>'
    )
    channel = (
        '<text x="14" y="36" fill="#8ff0ff">#maps-night</text>'
        '<text x="14" y="60" fill="#cfe4ff">3 lanterns lit</text>'
        '<rect x="14" y="72" width="120" height="9" rx="3" fill="#6fd6ff" opacity="0.5"/>'
    )
    invite = (
        '<text x="14" y="36" fill="#ffd27a">friend invite?</text>'
        '<text x="14" y="60" fill="#cfe4ff">from Aya</text>'
        '<rect x="14" y="76" width="74" height="26" rx="6" fill="#d6a854"/>'
        '<text x="22" y="94" fill="#0a1430" font-weight="700">accept</text>'
        '<rect x="98" y="76" width="64" height="26" rx="6" fill="none" stroke="#9fb8e0" stroke-width="1.5"/>'
        '<text x="108" y="94" fill="#9fb8e0">later</text>'
    )
    return f"""<g font-family="{MONO}" font-size="15">
    {card(604, 84, -3, 200, 100, friends)}
    {card(652, 244, 2, 170, 100, chat)}
    {card(1076, 76, 4, 170, 100, channel)}
    {card(1072, 236, -3, 178, 120, invite, amber=True)}
  </g>"""


# --- the pictures --------------------------------------------------------------------------


def scene(p: dict, height: int) -> str:
    """The 1280-wide scene the heroes and the banner share; the sea starts 182 above the bottom."""
    sea = height - 182
    return f"""<rect width="1280" height="{height}" fill="url(#sky)"/>
  <g {p['stars']}>
    {stars(11, 110, 1280, sea - 50)}
  </g>
  <rect y="{sea - 158}" width="1280" height="160" fill="url(#horizon)"/>
  <rect y="{sea}" width="1280" height="182" fill="url(#sea)"/>
  <path d="M0 {sea} L1280 {sea}" {p['shore']} stroke-width="2"/>
  <g {p['ripples']} stroke-linecap="round" stroke-width="2">
    <path d="M140 {sea + 42} h120 M320 {sea + 72} h80 M60 {sea + 102} h160 M420 {sea + 132} h110 M250 {sea + 154} h60"/>
  </g>

  <!-- friends' lanterns, far off -->
  {lantern(538, 44, 0.16, string=False, opacity=0.7)}
  {lantern(1196, 372, 0.2, string=False, opacity=0.8)}
  {lantern(700, 372, 0.15, string=False, opacity=0.65)}
  {lantern(826, 36, 0.12, string=False, opacity=0.55)}

  <!-- the lantern under the aetheryte, and its light on the water -->
  <ellipse cx="930" cy="{sea + 132}" rx="34" ry="150" fill="url(#refl)" filter="url(#blur)"/>
  <g stroke="#ffd27a" stroke-linecap="round" opacity="0.45" stroke-width="3">
    <path d="M896 {sea + 30} h68 M880 {sea + 60} h100 M906 {sea + 92} h48 M890 {sea + 126} h80"/>
  </g>
  {crystal(930, 118, 0.85)}
  {lantern(930, 186, 0.86)}

  {cards()}

  <text x="82" y="262" font-family="{SERIF}" font-size="84" font-weight="700" fill="url(#gold)" letter-spacing="2">{TITLE}</text>
  <path d="M84 300 H520" stroke="url(#goldH)" stroke-width="2"/>
  <path d="M302 292 l8 8 l-8 8 l-8 -8 Z" fill="#d6a854"/>
  <text x="86" y="350" font-family="{SERIF}" font-size="30" font-style="italic" fill="{p['tagline']}">{TAGLINE}</text>
  <text x="86" y="398" font-family="{MONO}" font-size="19" {p['subline']}>{SUBLINE}</text>

  <rect x="14" y="14" width="1252" height="{height - 28}" rx="18" fill="none" stroke="url(#gold)" stroke-width="3" opacity="0.8"/>"""


def hero(light: bool) -> str:
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640" viewBox="0 0 1280 640">
  <title>{TITLE}: {TAGLINE}</title>
  <defs>{defs(LIGHT if light else DARK)}
  </defs>
  {scene(LIGHT if light else DARK, 640)}
</svg>
"""


def banner() -> str:
    # 730x380 is a hair taller than 2:1, so the same scene gets 26 more units of sea.
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="730" height="380" viewBox="0 0 1280 666">
  <title>{TITLE}: {TAGLINE}</title>
  <defs>{defs(DARK)}
  </defs>
  {scene(DARK, 666)}
</svg>
"""


def icon() -> str:
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
  <title>{TITLE}</title>
  <defs>{defs(DARK)}
    <radialGradient id="bg" cx="50%" cy="42%" r="75%"><stop offset="0" stop-color="#1f3564"/><stop offset="0.55" stop-color="#0f1c3b"/><stop offset="1" stop-color="#060b1a"/></radialGradient>
    <clipPath id="frame"><rect width="512" height="512" rx="104"/></clipPath>
  </defs>
  <g clip-path="url(#frame)">
    <rect width="512" height="512" fill="url(#bg)"/>
    <g fill="#cfe8ff">
      <circle cx="84" cy="96" r="3"/><circle cx="132" cy="62" r="2"/><circle cx="420" cy="84" r="2.5"/>
      <circle cx="444" cy="200" r="2"/><circle cx="66" cy="230" r="2"/><circle cx="400" cy="440" r="1.8"/>
    </g>
    {crystal(256, 108, 1.08)}
    {lantern(256, 150, 1.0)}
    <rect x="12" y="12" width="488" height="488" rx="94" fill="none" stroke="url(#gold)" stroke-width="8"/>
    <rect x="26" y="26" width="460" height="460" rx="82" fill="none" stroke="#c99a4b" stroke-opacity="0.45" stroke-width="2"/>
  </g>
</svg>
"""


def main() -> int:
    (OUT / "readme").mkdir(parents=True, exist_ok=True)
    pictures = (
        ("icon", icon(), (512, 512)),
        ("banner", banner(), (730, 380)),
        ("readme/hero-dark", hero(False), (1280, 640)),
        ("readme/hero-light", hero(True), (1280, 640)),
    )
    for name, svg, (w, h) in pictures:
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
