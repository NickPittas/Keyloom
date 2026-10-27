"""Tests for output product selection."""
import pytest
import os
import cv2
import numpy as np

from backend.service import OutputConfig
from backend.service.frame_ops import FrameOpsMixin


def make_runtime(root, n=3, fg=0.5, alpha=0.25, checker=0.5, cutout=(0.5, 0.5, 0.5, 0.5)):
    dirs = {
        "alpha": str(root / "c_alpha_exr"),
        "fg": str(root / "c_fg_exr"),
        "checker": str(root / "c_checker_comp_despill050_exr"),
        "cutout": str(root / "c_cutout_exr"),
    }
    for index in range(n):
        for product, prefix, array in (
            ("alpha", "alpha", np.full((4, 4), alpha)),
            ("fg", "fg", np.full((4, 4, 3), fg)),
            ("checker", "checker_comp_despill050", np.full((4, 4, 3), checker)),
            ("cutout", "cutout", np.broadcast_to(np.array(cutout), (4, 4, 4))),
        ):
            os.makedirs(dirs[product], exist_ok=True)
            assert cv2.imwrite(os.path.join(dirs[product], f"{prefix}_{index:06d}.exr"), np.ascontiguousarray(array, dtype=np.float32))
    return dirs


class Svc(FrameOpsMixin):
    pass


def test_required_products():
    assert Svc._v2_required_products(OutputConfig()) == ["fg", "alpha", "checker", "cutout"]
    c = OutputConfig(fg_enabled=False, comp_enabled=False)
    assert Svc._v2_required_products(c) == ["alpha", "cutout"]
    c = OutputConfig(fg_enabled=False, matte_enabled=False, comp_enabled=False,
                     processed_enabled=False)
    with pytest.raises(ValueError):
        Svc._v2_required_products(c)
