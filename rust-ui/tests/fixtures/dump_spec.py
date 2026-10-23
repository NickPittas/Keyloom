"""Print the coordinator test suite's frozen SPEC fixture as JSON: `dump_spec.py path/to/tests/coordinator/specs.py`."""
import json, runpy, sys

print(json.dumps(runpy.run_path(sys.argv[1])["SPEC"]))
