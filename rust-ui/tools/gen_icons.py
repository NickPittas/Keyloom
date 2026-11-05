#!/usr/bin/env python3
"""Generate rust-ui/src/ui/icons_gen.rs from the design-bible icon sources (pure Python 3, no packages).

Sources (read-only):
  bible/specimen.base.html      const I (16 px), const I12 (12 px), const S12, const S14
  bible/parts/icons-small.html  hand-drawn <symbol id="NAME-12|NAME-14">; they override I12 for the same name
Regenerate:  python3 rust-ui/tools/gen_icons.py [bible_dir]
Output is deterministic. Anything the converter does not know (tag, attribute, path command) aborts.
"""
import math
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

BIBLE = Path(sys.argv[1] if len(sys.argv) > 1 else "/home/npittas/keyloom-round3/ui-audit/bible")
OUT = Path(__file__).resolve().parent.parent / "src" / "ui" / "icons_gen.rs"
ARC_STEP = 0.1   # max radians per flattened arc/curve piece (error < 0.01 unit at r 6)


def die(msg):
    sys.exit("gen_icons: " + msg)


def js_dict(text, start_marker):
    i = text.index(start_marker) + len(start_marker)
    j = text.index("\n}", i) if "\n}" in text[i:i + 60000] else die("unterminated " + start_marker)
    body = text[i:j]
    out = {}
    for m in re.finditer(r"(?:'([\w-]+)'|(\b[\w-]+\b))\s*:\s*'([^']*)'", body):
        out[m.group(1) or m.group(2)] = m.group(3)
    return out


def js_list(text, name):
    m = re.search(r"const %s=\[(.*?)\];" % name, text, re.S)
    return re.findall(r"'([\w-]+)'", m.group(1))


# ---------------------------------------------------------------- path parsing
NUM = re.compile(r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?")


def flatten_arc(p0, rx, ry, rot, large, sweep, p1):
    (x1, y1), (x2, y2) = p0, p1
    if rot != 0:
        die("rotated arc unsupported")
    if (x1, y1) == (x2, y2):
        return []
    if rx != ry:
        die("elliptical arc unsupported")
    r = abs(rx)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    lam = (dx * dx + dy * dy) / (r * r)
    if lam > 1:
        r *= math.sqrt(lam)
    sq = max(0.0, (r * r * (dx * dx + dy * dy) - 0) / 1)  # placeholder to keep formula readable below
    num = r * r * r * r - r * r * (dx * dx + dy * dy)  # = r^2*(r^2 - d^2) with rx=ry
    den = r * r * (dx * dx + dy * dy)
    coef = math.sqrt(max(0.0, num / den)) if den else 0.0
    if large == sweep:
        coef = -coef
    cxp, cyp = coef * (dy), coef * (-dx)  # rx=ry: (rx*y1'/ry, -ry*x1'/rx)
    cx, cy = cxp + (x1 + x2) / 2, cyp + (y1 + y2) / 2
    a1 = math.atan2(y1 - cy, x1 - cx)
    a2 = math.atan2(y2 - cy, x2 - cx)
    d = a2 - a1
    if sweep and d < 0:
        d += 2 * math.pi
    if not sweep and d > 0:
        d -= 2 * math.pi
    n = max(1, int(math.ceil(abs(d) / ARC_STEP)))
    return [(cx + r * math.cos(a1 + d * k / n), cy + r * math.sin(a1 + d * k / n), 1) for k in range(1, n + 1)]


def flatten_cubic(p0, c1, c2, p1):
    n = 16
    out = []
    for k in range(1, n + 1):
        t = k / n
        u = 1 - t
        out.append((u**3 * p0[0] + 3 * u * u * t * c1[0] + 3 * u * t * t * c2[0] + t**3 * p1[0],
                    u**3 * p0[1] + 3 * u * u * t * c1[1] + 3 * u * t * t * c2[1] + t**3 * p1[1], 1))
    return out


def parse_path(d):
    """-> list of (points, closed); points are (x, y, curved): curved=1 marks a vertex reached by a flattened arc/cubic piece."""
    pos, n = 0, len(d)
    subs = []
    cur, closed = None, False
    x = y = sx = sy = 0.0
    cmd = None
    last_c2 = None  # previous cubic's second control point, for S

    def ws():
        nonlocal pos
        while pos < n and d[pos] in " ,\t\n":
            pos += 1

    def num():
        nonlocal pos
        ws()
        m = NUM.match(d, pos)
        if not m:
            die("bad number in path %r at %d" % (d, pos))
        pos = m.end()
        return float(m.group())

    def flag():
        nonlocal pos
        ws()
        if d[pos] not in "01":
            die("bad arc flag in %r" % d)
        pos += 1
        return d[pos - 1] == "1"

    def more():
        ws()
        return pos < n and not d[pos].isalpha()

    while True:
        ws()
        if pos >= n:
            break
        if d[pos].isalpha():
            cmd = d[pos]
            pos += 1
            if cmd not in "MmLlHhVvCcSsAaZz":
                die("unsupported path command %r in %r" % (cmd, d))
            if cmd in "Zz":
                if cur:
                    subs.append((cur, True))
                cur = None
                x, y = sx, sy
                continue
        elif cmd is None:
            die("path must start with a command: " + d)
        rel = cmd.islower()
        c = cmd.upper()
        while True:
            if c == "M":
                nx, ny = num(), num()
                if rel:
                    nx, ny = x + nx, y + ny
                if cur:
                    subs.append((cur, False))
                cur = [(nx, ny, 0)]
                x, y = sx, sy = nx, ny
                c, cmd = "L", ("l" if rel else "L")  # implicit lineto after moveto
            elif c == "L":
                nx, ny = num(), num()
                if rel:
                    nx, ny = x + nx, y + ny
                cur = cur if cur is not None else [(x, y, 0)]
                cur.append((nx, ny, 0))
                x, y = nx, ny
            elif c == "H":
                nx = num() + (x if rel else 0)
                cur = cur if cur is not None else [(x, y, 0)]
                cur.append((nx, y, 0))
                x = nx
            elif c == "V":
                ny = num() + (y if rel else 0)
                cur = cur if cur is not None else [(x, y, 0)]
                cur.append((x, ny, 0))
                y = ny
            elif c == "C":
                v = [num() for _ in range(6)]
                if rel:
                    v = [v[i] + (x if i % 2 == 0 else y) for i in range(6)]
                cur = cur if cur is not None else [(x, y, 0)]
                cur += flatten_cubic((x, y), (v[0], v[1]), (v[2], v[3]), (v[4], v[5]))
                x, y = v[4], v[5]
                last_c2 = (v[2], v[3])
            elif c == "S":
                v = [num() for _ in range(4)]
                if rel:
                    v = [v[i] + (x if i % 2 == 0 else y) for i in range(4)]
                c1 = (2 * x - last_c2[0], 2 * y - last_c2[1]) if last_c2 else (x, y)
                cur = cur if cur is not None else [(x, y, 0)]
                cur += flatten_cubic((x, y), c1, (v[0], v[1]), (v[2], v[3]))
                x, y = v[2], v[3]
                last_c2 = (v[0], v[1])
                if not more():
                    break
                continue
            elif c == "A":
                rx, ry, rot = num(), num(), num()
                large, sweep = flag(), flag()
                nx, ny = num(), num()
                if rel:
                    nx, ny = x + nx, y + ny
                cur = cur if cur is not None else [(x, y, 0)]
                cur += flatten_arc((x, y), rx, ry, rot, large, sweep, (nx, ny))
                x, y = nx, ny
            else:
                die("unhandled " + c)
            if not more():
                break
    if cur:
        subs.append((cur, False))
    return subs


# ---------------------------------------------------------------- geometry helpers
def dedupe(pts):
    out = [pts[0]]
    for p in pts[1:]:
        if abs(p[0] - out[-1][0]) > 1e-9 or abs(p[1] - out[-1][1]) > 1e-9:
            out.append(p)
    return out


def xy(pts):
    return [(p[0], p[1]) for p in pts]


def rect_poly(x, y, w, h, r):
    if r <= 0:
        return [(x, y, 0), (x + w, y, 0), (x + w, y + h, 0), (x, y + h, 0)]
    pts = []
    for cx, cy, a0 in ((x + w - r, y + r, -math.pi / 2), (x + w - r, y + h - r, 0), (x + r, y + h - r, math.pi / 2), (x + r, y + r, math.pi)):
        n = max(2, int(math.ceil((math.pi / 2) / ARC_STEP)))
        for k in range(n + 1):
            a = a0 + (math.pi / 2) * k / n
            pts.append((cx + r * math.cos(a), cy + r * math.sin(a), 1 if k else 0))
    return pts


def dash(pts, closed, pattern):
    """Split a polyline into dash pieces (open polylines)."""
    path = pts + [pts[0][:2] + (pts[0][2] if False else 0,)] if closed else pts
    on = True
    idx = 0
    left = pattern[0]
    cur = [path[0]]
    out = []
    for a, b in zip(path, path[1:]):
        seg = math.hypot(b[0] - a[0], b[1] - a[1])
        t0 = 0.0
        while seg - t0 > left + 1e-9:
            t0 += left
            p = (a[0] + (b[0] - a[0]) * t0 / seg, a[1] + (b[1] - a[1]) * t0 / seg, b[2])
            if on:
                cur.append(p)
                out.append(cur)
                cur = []
            else:
                cur = [p]
            on = not on
            idx = (idx + 1) % len(pattern)
            left = pattern[idx]
        left -= seg - t0
        if on:
            cur.append(b)
    if on and len(cur) > 1:
        out.append(cur)
    return out


def triangulate(pts):
    """Ear clipping of a simple polygon -> flat list of triangle corners."""
    pts = dedupe(pts)
    if pts[0] == pts[-1]:
        pts = pts[:-1]
    area = sum(a[0] * b[1] - b[0] * a[1] for a, b in zip(pts, pts[1:] + pts[:1]))
    idx = list(range(len(pts)))
    if area < 0:
        idx.reverse()

    def cross(o, a, b):
        return (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])

    def inside(p, a, b, c):
        return cross(a, b, p) >= -1e-9 and cross(b, c, p) >= -1e-9 and cross(c, a, p) >= -1e-9

    tris = []
    guard = 0
    while len(idx) > 3 and guard < 10000:
        guard += 1
        for k in range(len(idx)):
            a, b, c = idx[k - 1], idx[k], idx[(k + 1) % len(idx)]
            if cross(pts[a], pts[b], pts[c]) <= 1e-12:
                continue
            if any(inside(pts[o], pts[a], pts[b], pts[c]) for o in idx if o not in (a, b, c)):
                continue
            tris += [pts[a], pts[b], pts[c]]
            idx.pop(k)
            break
        else:
            die("cannot triangulate polygon %r" % (pts,))
    if len(idx) == 3 and cross(pts[idx[0]], pts[idx[1]], pts[idx[2]]) > 1e-12:
        tris += [pts[i] for i in idx]
    return tris


# ---------------------------------------------------------------- svg -> prims
KNOWN = {"x", "y", "width", "height", "rx", "cx", "cy", "r", "d", "fill", "fill-opacity", "stroke", "stroke-dasharray"}


def f(el, name, default=None):
    v = el.get(name)
    return float(v) if v is not None else default


def convert(svg_inner):
    """-> list of ('line'|'poly'|'circle'|'tris'|'disc', data, paint). paint: Ink|Tone|Bg."""
    root = ET.fromstring("<g>" + svg_inner + "</g>")
    prims = []
    for el in root:
        for a in el.attrib:
            if a not in KNOWN:
                die("unsupported attribute %r on <%s>" % (a, el.tag))
        fill = el.get("fill", "none")
        opacity = el.get("fill-opacity")
        if fill == "none":
            fp = None
        elif fill == "currentColor":
            fp = "Tone" if opacity == ".35" else "Ink"
            if opacity not in (None, ".35"):
                die("fill-opacity %r unsupported" % opacity)
        elif fill == "var(--raised)":
            fp = "Bg"
        else:
            die("unsupported fill %r" % fill)
        stroke = el.get("stroke", "currentColor") != "none"
        if el.get("stroke", "currentColor") not in ("none", "currentColor"):
            die("unsupported stroke")
        dsh = [float(v) for v in el.get("stroke-dasharray").split()] if el.get("stroke-dasharray") else None
        shapes = []  # (points, closed) or circle
        tag = el.tag
        if tag == "rect":
            shapes.append((rect_poly(f(el, "x", 0), f(el, "y", 0), f(el, "width"), f(el, "height"), f(el, "rx", 0)), True))
        elif tag == "path":
            shapes += [(dedupe(p), c) for p, c in parse_path(el.get("d"))]
        elif tag == "circle":
            c = (f(el, "cx"), f(el, "cy"), f(el, "r"))
            if fp:
                prims.append(("disc", c, fp))
            if stroke:
                prims.append(("circle", c, "Ink"))
            continue
        else:
            die("unsupported element <%s>" % tag)
        for pts, closed in shapes:
            if fp and len(pts) >= 3:
                prims.append(("tris", triangulate(xy(pts)), fp))
            if stroke:
                if dsh:
                    for piece in dash(pts, closed, dsh):
                        prims.append(("line", piece, "Ink"))
                elif closed:
                    prims.append(("poly", pts, "Ink"))
                else:
                    prims.append(("line", pts, "Ink"))
    return prims


# ---------------------------------------------------------------- collect sources
def collect():
    html = (BIBLE / "specimen.base.html").read_text()
    small = (BIBLE / "parts" / "icons-small.html").read_text()
    i16 = js_dict(html, "const I={")
    for blk in re.finditer(r"Object\.assign\(I,\{(.*?)\n\}\);", html, re.S):
        i16.update(js_dict("X{" + blk.group(1) + "\n}", "X{"))
    i12 = js_dict(html, "const I12={")
    s12, s14 = js_list(html, "S12"), js_list(html, "S14")
    hand = {}
    for m in re.finditer(r'<symbol id="([\w-]+)-(12|14)" viewBox="0 0 (\d+) (\d+)">(.*?)</symbol>', small):
        if m.group(3) != m.group(2) or m.group(4) != m.group(2):
            die("symbol %s-%s has viewBox %s %s" % (m.group(1), m.group(2), m.group(3), m.group(4)))
        hand[(m.group(1), int(m.group(2)))] = m.group(5)
    names = list(i16)
    for n in s12 + s14:
        if n not in names:
            names.append(n)
    draw = {}
    for n in names:
        if n in i16:
            draw[(n, 16)] = i16[n]
        if n in s12:
            src = hand.get((n, 12)) or i12.get(n)
            if src is None:
                die("%s listed in S12 but has no 12 px drawing" % n)
            draw[(n, 12)] = src
        if n in s14:
            src = hand.get((n, 14))
            if src is None:
                die("%s listed in S14 but has no hand-drawn 14 px symbol" % n)
            draw[(n, 14)] = src
    return names, draw


def camel(n):
    return "".join(p.capitalize() for p in n.split("-"))


def fl(v):
    v = round(v + 0.0, 4)
    s = repr(v)
    if s.endswith(".0"):
        return s
    return s if "." in s else s + ".0"


def pts_rs(pts):
    return "&[" + ",".join("(%s,%s)" % (fl(x), fl(y)) for x, y in pts) + "]"


def lpts_rs(pts):
    return "&[" + ",".join("(%s,%s,%d)" % (fl(p[0]), fl(p[1]), p[2]) for p in pts) + "]"


def prim_rs(kind, data, paint):
    if kind == "line":
        return "P{s:S::Line(%s),p:Paint::Ink}" % lpts_rs(data)
    if kind == "poly":
        return "P{s:S::Poly(%s),p:Paint::Ink}" % lpts_rs(data)
    if kind == "circle":
        return "P{s:S::Circle(%s,%s,%s),p:Paint::Ink}" % tuple(fl(v) for v in data)
    if kind == "tris":
        return "P{s:S::Tris(%s),p:Paint::%s}" % (pts_rs(data), paint)
    if kind == "disc":
        return "P{s:S::Disc(%s,%s,%s),p:Paint::%s}" % (fl(data[0]), fl(data[1]), fl(data[2]), paint)
    die(kind)


def main():
    names, draw = collect()
    L = []
    L.append("// @generated by rust-ui/tools/gen_icons.py from the design bible (specimen.base.html + parts/icons-small.html).")
    L.append("// Regenerate: python3 rust-ui/tools/gen_icons.py   Do not edit by hand.")
    L.append("#![allow(clippy::all)]")
    L.append("use super::icons::{Paint, Prim as P, Shape as S};")
    L.append("")
    L.append("/// One variant per icon name (kebab-case in the bible -> CamelCase).")
    L.append("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]")
    L.append("pub enum Icon {")
    for n in names:
        L.append("    %s," % camel(n))
    L.append("}")
    L.append("")
    L.append("impl Icon {")
    L.append("    pub const ALL: [Icon; %d] = [%s];" % (len(names), ", ".join("Icon::" + camel(n) for n in names)))
    L.append("    /// The bible's kebab-case name.")
    L.append("    pub fn name(self) -> &'static str {")
    L.append("        match self {")
    for n in names:
        L.append('            Icon::%s => "%s",' % (camel(n), n))
    L.append("        }")
    L.append("    }")
    L.append("}")
    L.append("")
    L.append("/// Drawing primitives of `icon` on its own `grid` x `grid` unit grid (12, 14 or 16); `None` if that size does not exist.")
    L.append("pub fn drawing(icon: Icon, grid: u8) -> Option<&'static [P]> {")
    L.append("    match (icon, grid) {")
    for n in names:
        for g in (12, 14, 16):
            if (n, g) in draw:
                L.append("        (Icon::%s, %d) => Some(D_%s_%d)," % (camel(n), g, n.replace("-", "_").upper(), g))
    L.append("        _ => None,")
    L.append("    }")
    L.append("}")
    L.append("")
    for n in names:
        for g in (12, 14, 16):
            if (n, g) in draw:
                prims = convert(draw[(n, g)])
                if not prims:
                    die("%s-%d produced no primitives" % (n, g))
                L.append("const D_%s_%d: &[P] = &[%s];" % (n.replace("-", "_").upper(), g, ",".join(prim_rs(*p) for p in prims)))
    OUT.write_text("\n".join(L) + "\n")
    print("wrote %s: %d icons, %d drawings" % (OUT, len(names), len(draw)))


if __name__ == "__main__":
    main()
