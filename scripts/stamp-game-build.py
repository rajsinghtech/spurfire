#!/usr/bin/env python3
"""Stamp one exact source commit into the Godot project before exporting a client."""

from __future__ import annotations

import argparse
import os
import re
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PROJECT = ROOT / "game" / "project.godot"
SETTING = "config/build_commit="


class StampError(ValueError):
    """The source binding or project file is not exact."""


def stamp(project_file: Path, source_sha: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise StampError("source SHA must be 40 lowercase hexadecimal characters")

    try:
        original = project_file.read_text(encoding="utf-8")
    except OSError as error:
        raise StampError(f"cannot read project file: {error}") from error

    lines = original.splitlines(keepends=True)
    matches = [index for index, line in enumerate(lines) if line.startswith(SETTING)]
    if len(matches) != 1:
        raise StampError("project must contain exactly one build_commit setting")

    index = matches[0]
    newline = "\n" if lines[index].endswith("\n") else ""
    lines[index] = f'{SETTING}"{source_sha}"{newline}'
    stamped = "".join(lines)

    try:
        mode = project_file.stat().st_mode
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=project_file.parent,
            prefix=f".{project_file.name}.",
            delete=False,
        ) as temporary:
            temporary.write(stamped)
            temporary_path = Path(temporary.name)
        os.chmod(temporary_path, mode)
        os.replace(temporary_path, project_file)
    except OSError as error:
        raise StampError(f"cannot stamp project file: {error}") from error


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_sha")
    parser.add_argument("--project-file", type=Path, default=DEFAULT_PROJECT)
    args = parser.parse_args(argv)
    try:
        stamp(args.project_file, args.source_sha)
    except StampError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
