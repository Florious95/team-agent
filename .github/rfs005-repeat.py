"""Bounded, sequential original-test/control experiments; no production hooks."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

mode, binary, count = sys.argv[1], sys.argv[2], int(sys.argv[3])
root = Path("rfs005-evidence") / mode
root.mkdir(parents=True, exist_ok=True)
name = "cases::rfs_restart_false_success_contract::rfs_refused_dirty_topology_event_precedes_any_spawn_argv_event"
rows = []
for index in range(count):
    case = root / f"{index:03d}"
    case.mkdir()
    env = os.environ.copy()
    if mode != "baseline":
        env.update(RFS005_MODE=mode, RFS005_HOLD_MS="120", RFS005_RECEIPT_DIR=str(case.resolve()))
    start = time.time()
    try:
        with (case / "test.log").open("w") as log:
            result = subprocess.run([binary, name, "--exact", "--nocapture", "--test-threads=1"], env=env,
                                    stdout=log, stderr=subprocess.STDOUT, timeout=25)
        code = result.returncode
    except subprocess.TimeoutExpired:
        code = 124
    text = (case / "test.log").read_text()
    row = dict(index=index, exit=code, elapsed_seconds=round(time.time()-start, 6),
               actually_ran="running 1 test" in text,
               missing_refusal="R2 RED: missing restart.refused_dirty_topology" in text)
    rows.append(row)
    print(mode, row, flush=True)
    (root / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
summary = dict(mode=mode, total=len(rows), passed=sum(r['exit']==0 and r['actually_ran'] for r in rows),
               failed=sum(r['exit']!=0 for r in rows), failures=[r for r in rows if r['exit']!=0])
(root / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(json.dumps(summary), flush=True)
if not all(r['actually_ran'] and r['exit'] != 124 for r in rows):
    sys.exit(2)
if mode == "quiesced" and summary['failed']:
    sys.exit(1)
