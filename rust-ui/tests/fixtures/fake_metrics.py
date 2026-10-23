"""Stand-in for backend/system_metrics.py: mode picks the behaviour. Never touches the machine's real counters."""
import json, sys, time

mode = sys.argv[1]
LINE = {"v": 1, "type": "system_metrics", "cpu_percent": 42.0, "ram_used_bytes": 4 << 30, "ram_total_bytes": 16 << 30,
        "gpus": [{"uuid": "GPU-fake", "index": 0, "name": "Fake GPU", "utilization_percent": 55.0,
                  "vram_used_bytes": 2 << 30, "vram_total_bytes": 8 << 30}], "errors": {}}

def emit(seq):
    sys.stdout.write(json.dumps({**LINE, "seq": seq}) + "\n"); sys.stdout.flush()

if mode == "run":
    n = 0
    while True:
        n += 1; emit(n); time.sleep(0.1)
elif mode == "one":  # one sample, then silence (goes stale), process stays alive
    emit(1); time.sleep(60)
elif mode == "crash":
    sys.stderr.write("boom: no nvml\n"); sys.exit(3)
elif mode == "garbage":
    for _ in range(5):
        sys.stdout.write("not json\n"); sys.stdout.flush(); time.sleep(0.05)
    time.sleep(60)
elif mode == "flood":  # a very chatty stderr must neither block the helper nor grow the UI's memory
    for _ in range(2000):
        sys.stderr.write("x" * 200 + "\n")
    sys.stderr.flush(); emit(1); time.sleep(60)
elif mode == "utf8-stderr":  # 'a' + 1500 two-byte characters on stderr: the 2048-byte cut lands inside a character
    sys.stderr.buffer.write(("a" + "é" * 1500).encode()); sys.stderr.flush(); emit(1); time.sleep(60)
elif mode == "utf8-crash":
    sys.stderr.buffer.write(("a" + "é" * 1500).encode()); sys.stderr.flush(); sys.exit(3)
