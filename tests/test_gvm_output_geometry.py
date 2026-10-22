"""Exercise the real GVM reader/postprocess/writer with a tiny fake denoiser."""
from types import SimpleNamespace

import numpy as np
from PIL import Image
import pytest
import torch

from gvm_core.wrapper import GVMProcessor


@pytest.mark.parametrize('size', [(64, 64), (96, 64)])
def test_sequence_output_keeps_source_geometry_and_dotted_stems(tmp_path, size):
    source, out = tmp_path / 'Frames', tmp_path / 'AlphaHint'
    source.mkdir()
    for i in range(2):
        Image.fromarray(np.full((size[1], size[0], 3), 100, np.uint8)).save(
            source / f'shot.{i:04}.png')
    processor = GVMProcessor.__new__(GVMProcessor)
    processor.device = torch.device('cpu')
    processor._dtype = torch.float32
    processor.pipe = lambda frames, **kw: SimpleNamespace(
        image=frames, alpha=torch.full_like(frames[:, :1], .5))
    processor.process_sequence(source, tmp_path, num_frames_per_batch=1,
                               write_video=False, direct_output_dir=out)
    assert sorted(p.name for p in out.iterdir()) == ['shot.0000.png', 'shot.0001.png']
    for p in out.iterdir():
        with Image.open(p) as image:
            assert image.size == size
            assert abs(float(np.asarray(image).mean()) - 127) <= 1
