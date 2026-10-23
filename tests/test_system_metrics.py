"""backend/system_metrics.py: pure parsers, CPU delta, null-not-zero failures and the process contract. No GPU or NVML needed."""
import json
import subprocess
import sys
import unittest
from pathlib import Path

import importlib.util

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "backend" / "system_metrics.py"
# Loaded by path: importing the `backend` package would pull in cv2/torch, which the sampler must never do.
_spec = importlib.util.spec_from_file_location("system_metrics_under_test", SCRIPT)
sm = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = sm
_spec.loader.exec_module(sm)


class Os:
    def __init__(self, cpu=((100, 400), (150, 500)), mem=(2 << 30, 8 << 30)):
        self.cpu, self.mem, self.n = list(cpu), mem, 0

    def cpu_counters(self):
        v = self.cpu[min(self.n, len(self.cpu) - 1)]
        self.n += 1
        if isinstance(v, Exception):
            raise v
        return v

    def memory(self):
        if isinstance(self.mem, Exception):
            raise self.mem
        return self.mem


class Gpus:
    def __init__(self, result):
        self.result = result

    def read(self):
        if isinstance(self.result, Exception):
            raise self.result
        return self.result

    def close(self):
        pass


class Parsers(unittest.TestCase):
    def test_proc_stat_and_delta(self):
        a = sm.parse_proc_stat("cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 1 2 3 4 5\n")
        self.assertEqual(a, (80, 100))
        b = sm.parse_proc_stat("cpu  20 0 20 130 30 0 0 0 0 0\n")
        self.assertEqual(sm.cpu_percent(a, b), 20.0)  # d_total 100, d_idle 80
        self.assertIsNone(sm.cpu_percent(None, b), "first sample is warming, not 0")
        self.assertIsNone(sm.cpu_percent(b, b), "no progress is unknown, not 0 or 100")
        self.assertIsNone(sm.cpu_percent(b, (b[0] - 1, b[1] - 1)))
        self.assertIsNone(sm.parse_proc_stat("garbage"))
        self.assertIsNone(sm.parse_proc_stat("cpu  a b c d e"))

    def test_meminfo(self):
        self.assertEqual(sm.parse_meminfo("MemTotal: 1000 kB\nMemFree: 1 kB\nMemAvailable: 400 kB\n"), (600 * 1024, 1000 * 1024))
        self.assertIsNone(sm.parse_meminfo("MemTotal: 1000 kB\n"))
        self.assertIsNone(sm.parse_meminfo("MemTotal: 10 kB\nMemAvailable: 400 kB\n"))
        self.assertIsNone(sm.parse_meminfo("MemTotal: x kB\nMemAvailable: 4 kB\n"))


class Sampling(unittest.TestCase):
    GPU = [{"uuid": "GPU-1", "index": 0, "name": "RTX", "utilization_percent": 40.0, "vram_used_bytes": 1, "vram_total_bytes": 2}]

    def test_first_sample_warms_then_reports(self):
        s = sm.Sampler(Os(cpu=((100, 400), (150, 500))), Gpus((self.GPU, None)))
        first = s.sample()
        self.assertIsNone(first["cpu_percent"])
        self.assertEqual(first["errors"]["cpu"], "warming up")
        self.assertEqual((first["ram_used_bytes"], first["ram_total_bytes"]), (2 << 30, 8 << 30))
        self.assertEqual(first["gpus"], self.GPU)
        second = s.sample()
        self.assertEqual((second["seq"], second["cpu_percent"]), (2, 50.0))
        self.assertNotIn("cpu", second["errors"])
        self.assertEqual((first["v"], first["type"]), (1, "system_metrics"))

    def test_one_failing_source_never_zeroes_or_kills_the_others(self):
        s = sm.Sampler(Os(cpu=(RuntimeError("no stat"),), mem=OSError("no mem")), Gpus((None, "NVML unavailable")))
        r = s.sample()
        self.assertEqual((r["cpu_percent"], r["ram_used_bytes"], r["ram_total_bytes"], r["gpus"]), (None,) * 4)
        self.assertEqual(set(r["errors"]), {"cpu", "ram", "gpu"})
        ok = sm.Sampler(Os(), Gpus(RuntimeError("boom"))).sample()
        self.assertIsNone(ok["gpus"])
        self.assertIn("boom", ok["errors"]["gpu"])
        self.assertEqual(ok["ram_total_bytes"], 8 << 30)
        json.dumps(r)

    def test_gpu_fields_fail_independently(self):
        class N:
            def nvmlInit(self): pass
            def nvmlShutdown(self): pass
            def nvmlDeviceGetCount(self): return 3
            def nvmlDeviceGetHandleByIndex(self, i): return i
            def nvmlDeviceGetUUID(self, h):
                if h == 1:
                    raise RuntimeError("lost")
                return b"GPU-%d" % h
            def nvmlDeviceGetName(self, h): return b"RTX 4090"
            def nvmlDeviceGetUtilizationRates(self, h): raise RuntimeError("no util")
            def nvmlDeviceGetMemoryInfo(self, h):
                class M: used, total = 5, 10
                return M()
        devs, why = sm._Gpus(N()).read()
        self.assertEqual([d["uuid"] for d in devs], ["GPU-0", "GPU-2"], "a device without identity is skipped, others stay")
        self.assertEqual([d["index"] for d in devs], [0, 2], "NVML index kept, uuid is the identity")
        self.assertIsNone(devs[0]["utilization_percent"])
        self.assertEqual((devs[0]["name"], devs[0]["vram_used_bytes"], devs[0]["vram_total_bytes"]), ("RTX 4090", 5, 10))
        self.assertIn("GPU 1", why)

    def test_missing_nvml_is_unavailable_and_not_retried_every_tick(self):
        class Boom:
            calls = 0
            def nvmlInit(self):
                Boom.calls += 1
                raise OSError("no driver")
        g = sm._Gpus(Boom())
        for _ in range(5):
            devs, why = g.read()
            self.assertIsNone(devs)
            self.assertIn("NVML unavailable", why)
        self.assertEqual(Boom.calls, 1)

    def test_text_is_bounded(self):
        self.assertLessEqual(len(sm._short("x" * 1000)), sm.MAX_TEXT)


class Process(unittest.TestCase):
    def test_emits_json_lines_and_exits_after_count(self):
        p = subprocess.run([sys.executable, "-I", str(SCRIPT), "--interval", "0.5", "--count", "2"],
                           cwd=ROOT, capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 0, p.stderr)
        lines = [json.loads(x) for x in p.stdout.splitlines()]
        self.assertEqual([x["seq"] for x in lines], [1, 2])
        for x in lines:
            self.assertEqual(x["type"], "system_metrics")


    def test_the_child_never_imports_torch_cv2_or_numpy(self):
        # Verified on the CHILD (the test process itself may have imported them via conftest).
        p = subprocess.run([sys.executable, "-I", "-X", "importtime", str(SCRIPT), "--interval", "0.5", "--count", "1"],
                           cwd=ROOT, capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 0, p.stderr[-500:])
        imported = [ln.split("|")[-1].strip() for ln in p.stderr.splitlines() if ln.startswith("import time:")]
        self.assertTrue(imported, "importtime produced no data")
        bad = [m for m in imported if m.split(".")[0] in ("torch", "cv2", "numpy", "backend")]
        self.assertEqual(bad, [])


if __name__ == "__main__":
    unittest.main()
