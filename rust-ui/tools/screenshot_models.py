"""Screenshot sandbox only: create sparse files of the catalog size for every model, under $HOME (run with HOME=/tmp)."""
import os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from backend.model_catalog import load_catalog
from backend.model_status import Roots, StatusChecker

assert os.environ.get("HOME", "").startswith("/tmp"), "refusing to touch a real home"
cat = load_catalog()
chk = StatusChecker(cat, Roots.defaults())
for art in cat.artifacts.values():
    for f in art.required_files():
        p = chk.file_path(art, f)
        if p is None or not str(p).startswith(os.environ["HOME"]) or f.size is None:
            continue
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, "wb") as fh:
            fh.truncate(f.size)
for art in cat.artifacts.values():
    print(art.id, chk.check_artifact(art).get("present"))
