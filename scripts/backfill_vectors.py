#!/usr/bin/env python3
"""DEPRECATED compatibility shim for the canonical Rust vector backfill.

This entrypoint contains no SQLite or provider implementation. It forwards its
legacy arguments to:
    tachi backfill-vectors --db <db_path>

Usage:
    python3 scripts/backfill_vectors.py <db_path> [--dry-run]
        [--batch-size 64] [--include-cache]

Requires the ``tachi`` executable on PATH and fails loudly when unavailable.
"""

import argparse
import shutil
import subprocess
import sys


DEFAULT_BATCH_SIZE = 64


def main() -> int:
    parser = argparse.ArgumentParser(
        description="DEPRECATED: forward vector backfill to the canonical tachi CLI"
    )
    parser.add_argument("db_path", help="Path to the SQLite memory DB")
    parser.add_argument("--dry-run", action="store_true", help="Only count; do not embed")
    parser.add_argument(
        "--batch-size",
        type=int,
        default=DEFAULT_BATCH_SIZE,
        help="Batch size forwarded to tachi",
    )
    parser.add_argument(
        "--include-cache",
        action="store_true",
        help="Include ephemeral recall-cache rows",
    )
    args = parser.parse_args()

    tachi = shutil.which("tachi")
    if tachi is None:
        print(
            "ERROR: deprecated backfill_vectors.py requires the canonical "
            "'tachi' executable on PATH",
            file=sys.stderr,
        )
        return 127

    command = [
        tachi,
        "backfill-vectors",
        "--db",
        args.db_path,
        "--batch-size",
        str(args.batch_size),
    ]
    if args.dry_run:
        command.append("--dry-run")
    if args.include_cache:
        command.append("--include-cache")

    try:
        return subprocess.run(command, check=False).returncode
    except OSError as error:
        print(f"ERROR: failed to execute canonical tachi backfill: {error}", file=sys.stderr)
        return 127


if __name__ == "__main__":
    sys.exit(main())
