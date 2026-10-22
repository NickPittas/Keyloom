"""Manual real-worker GPU acceptance, one route per bounded invocation.

Run through rust-ui/tools/gpu-isolated-test.sh with a 14-second outer timeout.
Pass one kind below. Fixtures live only in the wrapper's /tmp; no user media.
HF_HUB_CACHE must name the verified managed cache when the wrapper clears HOME.
"""
import argparse
import json
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time

import cv2
import numpy as np

REPO = Path(__file__).resolve().parent.parent
KINDS = ('birefnet', 'gvm', 'sam_preview', 'sam_track', 'matanyone2', 'videomama')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('kind', choices=KINDS)
    kind = parser.parse_args().kind
    root = Path('/tmp/task-smoke/clip')
    frames = root / 'Frames'
    frames.mkdir(parents=True)
    y, x = np.mgrid[:256, :256]
    mask = (x - 128) ** 2 + (y - 128) ** 2 < 70 ** 2
    for i in range(4):
        path = frames / f'shot.{i:04}.png'
        if i not in (1, 2):
            path.write_bytes(b'POISON')
            continue
        im = np.full((256, 256, 3), (30, 180, 30), np.uint8)
        im[mask] = (40, 70, 220)
        assert cv2.imwrite(str(path), im)
        if kind in ('matanyone2', 'videomama'):
            masks = root / 'VideoMamaMaskHint'
            masks.mkdir(exist_ok=True)
            assert cv2.imwrite(str(masks / path.name), mask.astype('uint8') * 255)
    prompts = {'points': [{'x': 128, 'y': 128, 'label': 1}], 'boxes': []}
    params = {
        'birefnet': {'variant': 'Matting'},
        'sam_track': {'tracker_model': 'small', 'frames': {'shot.0001': prompts}},
        'sam_preview': {'tracker_model': 'small', 'frame_stem': 'shot.0001', 'prompts': prompts},
        'videomama': {'chunk_size': 2},
    }.get(kind, {})
    destination = root / ('VideoMamaMaskHint' if kind == 'sam_track' else 'AlphaHint')
    job = dict(job_id='real-smoke', clip_id='smoke/clip', clip_root=str(root), kind=kind,
               range=dict(in_point=1, out_point=2, in_stem='shot.0001', out_stem='shot.0002'),
               params=params, staging_dir='/tmp/task-smoke/staging',
               publish_dir=str(destination) if kind != 'sam_preview' else None,
               require={'protocol': 1, 'models': {}})
    proc = subprocess.Popen([sys.executable, '-B', '-m', 'backend.worker', '--gpu', '0'],
                            cwd=REPO, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True)
    messages, errors = queue.Queue(), []
    threading.Thread(target=lambda: [messages.put(line) for line in proc.stdout], daemon=True).start()
    threading.Thread(target=lambda: errors.extend(proc.stderr), daemon=True).start()
    deadline = time.monotonic() + 11
    events = []

    def read():
        event = json.loads(messages.get(timeout=max(.01, deadline - time.monotonic())))
        events.append(event)
        print(json.dumps(event), flush=True)
        return event

    try:
        hello = read()
        assert hello['event'] == 'hello' and hello['capabilities']['tasks'][kind], hello
        proc.stdin.write(json.dumps({'v': 1, 'cmd': 'task', 'job': job}) + '\n')
        proc.stdin.flush()
        while True:
            result = read()
            if result['event'] in ('done', 'rejected'):
                break
        assert result.get('outcome') == 'complete', (result, errors[-15:])
        proc.stdin.close()
        assert proc.wait(timeout=max(.01, deadline - time.monotonic())) == 0
        if kind == 'sam_preview':
            preview = next(e for e in events if e['event'] == 'preview')
            output = cv2.imread(preview['mask_png'], cv2.IMREAD_UNCHANGED)
            assert output is not None and output.shape[:2] == (256, 256)
        else:
            for i in (1, 2):
                output = cv2.imread(str(destination / f'shot.{i:04}.png'), cv2.IMREAD_UNCHANGED)
                assert output is not None and output.shape[:2] == (256, 256)
                assert np.isfinite(output).all()
            assert sorted(p.stem for p in destination.glob('*.png')) == ['shot.0001', 'shot.0002']
            assert not (root / '.corridorkey_hint.lock').exists()
        print('REAL_WORKER_TASK_OK', kind, flush=True)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=1)
        if errors:
            print(''.join(errors[-10:]), file=sys.stderr)


if __name__ == '__main__':
    main()
