"""Variant tables the app, the model helper and the executors share (torch-free): BiRefNet display name -> repo suffix; SAM tracker -> catalog id."""
from __future__ import annotations

BIREFNET_MODELS: dict[str, str] = {
    "Matting": "BiRefNet-matting",
    "Matting HR": "BiRefNet_HR-Matting",
    "Matting Lite": "BiRefNet_lite-matting",
    "Matting Dynamic": "BiRefNet_dynamic-matting",
    "General": "BiRefNet",
    "General HR": "BiRefNet_HR",
    "General Lite": "BiRefNet_lite",
    "General Lite 2K": "BiRefNet_lite-2K",
    "General Dynamic": "BiRefNet_dynamic",
    "General 512": "BiRefNet_512x512",
    "Portrait": "BiRefNet-portrait",
    "DIS": "BiRefNet-DIS5K",
    "HRSOD": "BiRefNet-HRSOD",
    "COD": "BiRefNet-COD",
    "DIS TR_TEs": "BiRefNet-DIS5K-TR_TEs",
    "General Legacy": "BiRefNet-legacy",
}

SAM_ARTIFACTS: dict[str, str] = {"small": "sam2-small", "base_plus": "sam2-base-plus", "large": "sam2-large"}
