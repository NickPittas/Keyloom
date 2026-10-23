"""Shared tiny test fixtures (frozen Key spec + capabilities); import-only, no production code."""

SPEC = {
    "protocol": 1, "kind": "key", "mode": "new", "preset_name": "Quality", "clip_id": "clip-1",
    "clip_root": "shared:projects/Show A/shot 010", "range": {"in_point": 0, "out_point": 99, "in_stem": "f0000", "out_stem": "f0099"},
    "params": {"input_is_linear": False, "use_alpha_hint": False, "hint_quality": 0.95, "carry_hint": True,
               "carry_hint_quality": 0.95, "vram_profile": "auto", "despill_strength": 0.5, "cutout_linear": False,
               "hint_mode": "none", "temporal_window": "1/1"},
    "output_config": {"fg_enabled": True, "fg_format": "exr", "matte_enabled": True, "matte_format": "exr",
                      "comp_enabled": False, "comp_format": "png", "processed_enabled": False, "processed_format": "exr",
                      "exr_compression": "dwab"},
    "hint_keyframes_abs": None, "manifest_dir": "shared:out/Show A/shot 010/.ck",
    "output": {"root": "shared:out", "folder_template": "{project}/{name}/{output}", "file_template": "{name}_{output}_####",
               "values": {"project": "Show A", "name": "shot 010", "resolution": "4K", "date": "2026-10-23", "version": 1}},
    "publish": "final", "require": {"protocol": 1, "models": {"corridorkey": None}},
}
CAPS = {"protocol": 1, "kinds": ["key"], "maps": ["projects", "out"], "models": {"corridorkey": "abc"}}
