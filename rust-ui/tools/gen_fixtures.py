#!/usr/bin/env python3
"""Regenerate the SYNTHETIC EXR fixtures in tests/fixtures (not real footage).

    ../.venv/bin/python tools/gen_fixtures.py

Content is a deterministic gradient: value(x, y) = (x + y * W) / (W * H) for Y/R, reversed for G, constant for B/A,
so tests can compare exact (lossless) or approximate (DWAB, lossy) results. Rust cannot encode DWAB, hence Python.
"""
import pathlib
import numpy as np
import OpenEXR

W, H = 64, 32
OUT = pathlib.Path(__file__).resolve().parent.parent / "tests" / "fixtures"
OUT.mkdir(parents=True, exist_ok=True)
base = (np.arange(W * H, dtype=np.float32) / (W * H)).reshape(H, W)
COMP = {"dwab": OpenEXR.DWAB_COMPRESSION, "piz": OpenEXR.PIZ_COMPRESSION, "zip": OpenEXR.ZIP_COMPRESSION, "none": OpenEXR.NO_COMPRESSION}


def write(name, comp, channels, dtype):
    chans = {k: v.astype(dtype) for k, v in channels.items()}
    OpenEXR.File({"compression": COMP[comp], "type": OpenEXR.scanlineimage}, chans).write(str(OUT / name))


for comp in COMP:
    write(f"y_{comp}.exr", comp, {"Y": base}, np.float32)
    write(f"rgb_{comp}.exr", comp, {"R": base, "G": base[::-1, ::-1], "B": np.full_like(base, 0.25)}, np.float16)
    write(f"rgba_{comp}.exr", comp, {"R": base, "G": base[::-1, ::-1], "B": np.full_like(base, 0.25), "A": np.full_like(base, 0.5)}, np.float16)
print("wrote", len(list(OUT.glob("*.exr"))), "files to", OUT)
