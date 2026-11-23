#!/usr/bin/env python3
from __future__ import annotations

import os
import stat
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RELEASE_SCRIPT = ROOT / ".github/scripts/check-release-parity.sh"


def run(command: list[str], cwd: Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def git(cwd: Path, *args: str) -> None:
    output = run(["git", *args], cwd, os.environ.copy())
    if output.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} failed:\nstdout={output.stdout}\nstderr={output.stderr}"
        )


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="kite-release-parity-") as raw:
        repo = Path(raw)
        fake_bin = repo / "fake-bin"
        fake_bin.mkdir()
        fake_gh = fake_bin / "gh"
        fake_gh.write_text('#!/bin/sh\nprintf "%s\\n" "$FAKE_OPEN_PARITY_PRS"\n')
        fake_gh.chmod(fake_gh.stat().st_mode | stat.S_IXUSR)

        git(repo, "init", "--quiet", "-b", "main")
        git(repo, "config", "user.name", "Kite Test")
        git(repo, "config", "user.email", "kite@example.invalid")
        (repo / "README.md").write_text("# fixture\n")
        git(repo, "add", "README.md")
        git(repo, "commit", "--quiet", "-m", "base")
        git(repo, "tag", "v0.0.1")

        notes = repo / "docs" / "RELEASE_NOTES_v0.0.2.md"
        notes.parent.mkdir()
        notes.write_text("# 0.0.2\n\n- Windows: fixture\n- macOS: fixture\n")
        git(repo, "add", "docs/RELEASE_NOTES_v0.0.2.md")
        git(repo, "commit", "--quiet", "-m", "release notes")

        env = os.environ.copy()
        env["PATH"] = f"{fake_bin}:{env['PATH']}"
        env["GH_TOKEN"] = "fixture-token"
        env["FAKE_OPEN_PARITY_PRS"] = "1"
        blocked = run(["bash", str(RELEASE_SCRIPT), "v0.0.2"], repo, env)
        combined = blocked.stdout + blocked.stderr
        if blocked.returncode == 0 or "open PR(s) still carry parity:required" not in combined:
            raise AssertionError(
                "release parity gate did not block an open parity PR:\n"
                f"stdout={blocked.stdout}\nstderr={blocked.stderr}"
            )

        env["FAKE_OPEN_PARITY_PRS"] = "0"
        passed = run(["bash", str(RELEASE_SCRIPT), "v0.0.2"], repo, env)
        if passed.returncode != 0:
            raise AssertionError(
                "release parity gate failed with no open parity PR:\n"
                f"stdout={passed.stdout}\nstderr={passed.stderr}"
            )

    print("Release parity gate test passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
