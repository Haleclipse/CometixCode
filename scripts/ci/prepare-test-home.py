#!/usr/bin/env python3
"""Rebuild the scratch config home that every isolated test run uses.

This is the single implementation behind `just _prep` and the CI test job. It
must stay byte-for-byte equivalent to what the justfile used to inline:

  - `target/test-home` is removed and recreated, so no test inherits state
    another run left behind;
  - `state.json` carries a hash-independent trust entry for the tracked
    `tests/fixtures/isolated-project`, serialized through a JSON encoder so
    path text is never interpolated into JSON syntax.

Run from the repository root (or pass --root).
"""

import argparse
import json
import pathlib
import shutil
import sys


def prepare(root: pathlib.Path) -> pathlib.Path:
    home = root / "target" / "test-home"
    project = (root / "tests" / "fixtures" / "isolated-project").resolve(strict=True)

    shutil.rmtree(home, ignore_errors=True)
    home.mkdir(parents=True)

    payload = {"projects": {str(project): {"hasTrustDialogAccepted": True}}}
    (home / "state.json").write_text(
        json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n"
    )
    return home


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root (default: cwd)")
    args = parser.parse_args()
    home = prepare(pathlib.Path(args.root).resolve())
    print(home)
    return 0


if __name__ == "__main__":
    sys.exit(main())
