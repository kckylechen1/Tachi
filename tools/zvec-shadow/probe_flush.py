#!/usr/bin/env python3
"""zvec flush/commit durability probe (tachi#683 Phase 0, G2).

Question: after calling the zvec explicit `flush()` API, does a hard
kill (SIGKILL, i.e. no atexit/finalizer/WAL-checkpoint-on-close chance to
run) preserve the flushed data? And conversely, is data inserted *after*
the last flush lost on the same kill?

Scope limit (do not over-read the result): SIGKILL terminates the process
but does NOT drop the OS page cache -- data flush() handed to the kernel
survives the kill even if it was never fsync'd to stable storage. So this
probe characterizes flush() as a **process-crash-level** durability
boundary only. It says NOTHING about power-loss / kernel-panic durability
(whether flush() fsyncs is uncharacterized here and would need a different
rig -- e.g. a VM with forced power-off -- to test).

Method: each scenario runs a fresh **child process** that does exactly the
prescribed sequence of insert()/flush() calls and then sends SIGKILL to
itself (`os.kill(os.getpid(), signal.SIGKILL)`) as its very last action —
no `finally`, no `atexit`, no graceful shutdown path can run. The parent
process (this script) waits for the child, confirms it actually died by
SIGKILL (not a normal exit — that would mean the kill point was never
reached), then reopens the same on-disk collection **read-only** in a
separate process and reports `doc_count` plus which doc ids survived.

This never touches a live Tachi memory.db — it only creates scratch zvec
collections under a temp directory.

Usage:
    python3 probe_flush.py                 # run all scenarios, print + write FINDINGS-flush.json
    python3 probe_flush.py --keep           # keep scratch dirs for manual inspection

Requires: the zvec pip package (verified working at zvec==0.5.1 during the
tachi#683 spike, see ~/.cache/zvec-spike/). Not part of the sidecar's
runtime dependency footprint check — this is a one-off investigation
script, safe to re-run any time.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import time

# The child scenario program. Templated with {db} and {steps}; {steps} is a
# python snippet built from a list of ("insert", n) / ("flush",) tuples.
CHILD_TEMPLATE = """
import os, numpy as np
import zvec
from zvec import (CollectionSchema, VectorSchema, FieldSchema, DataType,
                   MetricType, FlatIndexParam, FtsIndexParam, Doc,
                   create_and_open, open as zv_open)

DB = {db!r}
zvec.init(log_level=zvec.LogLevel.WARN)

schema = CollectionSchema(
    name="probe",
    fields=[FieldSchema(name="text", data_type=DataType.STRING,
                         index_param=FtsIndexParam(tokenizer_name="jieba"))],
    vectors=[VectorSchema(name="embedding", data_type=DataType.VECTOR_FP32,
                           dimension=8, index_param=FlatIndexParam(metric_type=MetricType.COSINE))],
)
if not os.path.exists(DB):
    col = create_and_open(DB, schema)
else:
    col = zv_open(DB)

next_id = [0]
def insert_batch(n):
    docs = []
    for _ in range(n):
        i = next_id[0]; next_id[0] += 1
        docs.append(Doc(id=f"d{{i}}",
            vectors={{"embedding": np.random.rand(8).astype(np.float32)}},
            fields={{"text": f"probe doc {{i}}"}}))
    col.insert(docs)

{steps}

# Last action: hard-kill ourselves. No cleanup, no WAL checkpoint on close,
# no atexit handler gets to run -- this is the kill -9 case, not sys.exit().
import signal as _signal
os.kill(os.getpid(), _signal.SIGKILL)
"""

READER_TEMPLATE = """
import os, json
import zvec
from zvec import open as zv_open, CollectionOption
DB = {db!r}
zvec.init(log_level=zvec.LogLevel.WARN)
opt = CollectionOption(read_only=True)
col = zv_open(DB, option=opt)
fetched = col.fetch([f"d{{i}}" for i in range({expect_max})], output_fields=["text"], include_vector=False)
print(json.dumps({{"doc_count": col.stats.doc_count, "present_ids": sorted(fetched.keys(), key=lambda s: int(s[1:]))}}))
"""


def run_child(py: str, db: str, steps_src: str) -> dict:
    src = CHILD_TEMPLATE.format(db=db, steps=textwrap.indent(steps_src, ""))
    t0 = time.time()
    proc = subprocess.run([py, "-c", src], capture_output=True, text=True, timeout=60)
    wall = time.time() - t0
    # A clean SIGKILL shows up as returncode == -9 on POSIX.
    return {
        "returncode": proc.returncode,
        "killed_by_sigkill": proc.returncode == -signal.SIGKILL,
        "wall_s": round(wall, 3),
        "stdout": proc.stdout[-2000:],
        "stderr": proc.stderr[-2000:],
    }


def read_back(py: str, db: str, expect_max: int) -> dict:
    src = READER_TEMPLATE.format(db=db, expect_max=expect_max)
    proc = subprocess.run([py, "-c", src], capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        return {"error": True, "returncode": proc.returncode, "stdout": proc.stdout, "stderr": proc.stderr[-2000:]}
    try:
        return json.loads(proc.stdout.strip().splitlines()[-1])
    except Exception as e:  # pragma: no cover - diagnostic path
        return {"error": True, "parse_exception": repr(e), "stdout": proc.stdout, "stderr": proc.stderr[-2000:]}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--python", default=os.path.join(os.path.dirname(__file__), "..", "..", "..", "zvec-spike", "venv", "bin", "python3"))
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "FINDINGS-flush.json"))
    args = ap.parse_args()

    py = args.python
    if not os.path.exists(py):
        # Fall back to whatever python3 is on PATH and hope zvec is importable
        # (e.g. CI image with zvec installed globally).
        py = shutil.which("python3") or sys.executable
    print(f"[probe_flush] using interpreter: {py}", flush=True)

    workdir = tempfile.mkdtemp(prefix="zvec-flush-probe-")
    results = {}
    try:
        # Scenario A: insert 50, flush(), then SIGKILL. Expect: all 50 survive.
        db_a = os.path.join(workdir, "scenario_a")
        r = run_child(py, db_a, "insert_batch(50)\ncol.flush()\n")
        rb = read_back(py, db_a, 50) if r["killed_by_sigkill"] else {"skipped": True}
        results["A_insert_then_flush_then_kill"] = {
            "expect": "all 50 docs survive (flush() is the process-crash durability boundary)",
            "child": r,
            "readback": rb,
            "pass": r["killed_by_sigkill"] and rb.get("doc_count") == 50 and len(rb.get("present_ids", [])) == 50,
        }

        # Scenario B: insert 50, NO flush, then SIGKILL. Expect: 0 survive
        # (or at least: fewer than 50 / collection may even fail to reopen
        # cleanly -- both are "unflushed data lost" outcomes worth recording
        # verbatim rather than assuming).
        db_b = os.path.join(workdir, "scenario_b")
        r = run_child(py, db_b, "insert_batch(50)\n")
        rb = read_back(py, db_b, 50) if r["killed_by_sigkill"] else {"skipped": True}
        results["B_insert_no_flush_then_kill"] = {
            "expect": "0 (or near-0) docs survive -- unflushed writes are lost on kill -9",
            "child": r,
            "readback": rb,
            "pass": r["killed_by_sigkill"] and rb.get("doc_count", -1) == 0,
        }

        # Scenario C: insert 50, flush(), insert 50 MORE (no flush), then
        # SIGKILL. Expect: exactly the first 50 survive -- flush() is a
        # precise durability boundary, not "best effort eventually".
        db_c = os.path.join(workdir, "scenario_c")
        r = run_child(py, db_c, "insert_batch(50)\ncol.flush()\ninsert_batch(50)\n")
        rb = read_back(py, db_c, 100) if r["killed_by_sigkill"] else {"skipped": True}
        present = set(rb.get("present_ids", []))
        first_50 = {f"d{i}" for i in range(50)}
        last_50 = {f"d{i}" for i in range(50, 100)}
        results["C_flush_boundary_precision"] = {
            "expect": "exactly ids d0..d49 survive, d50..d99 absent",
            "child": r,
            "readback": rb,
            "pass": r["killed_by_sigkill"] and present == first_50 and not (present & last_50),
        }
    finally:
        if args.keep:
            print(f"[probe_flush] scratch dirs kept at {workdir}")
        else:
            shutil.rmtree(workdir, ignore_errors=True)

    with open(args.out, "w") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)

    all_pass = all(v["pass"] for v in results.values())
    print(json.dumps(results, indent=2, ensure_ascii=False))
    print(f"\n[probe_flush] wrote {args.out}")
    print(f"[probe_flush] ALL SCENARIOS PASS = {all_pass}")
    return 0 if all_pass else 1


if __name__ == "__main__":
    raise SystemExit(main())
