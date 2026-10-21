"""Public deterministic repositories; never read a developer's project data."""

import os
import subprocess
from pathlib import Path


def environment():
    values = os.environ.copy()
    values.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                  GIT_AUTHOR_NAME="Benchmark", GIT_AUTHOR_EMAIL="bench@example.invalid",
                  GIT_COMMITTER_NAME="Benchmark", GIT_COMMITTER_EMAIL="bench@example.invalid",
                  GIT_AUTHOR_DATE="2026-01-01T00:00:00Z",
                  GIT_COMMITTER_DATE="2026-01-01T00:00:00Z", LC_ALL="C")
    return values


def git(repository, *arguments):
    return subprocess.check_output(["git", *arguments], cwd=repository,
                                   env=environment(), stderr=subprocess.PIPE).decode()


def build(repository):
    repository = Path(repository)
    repository.mkdir(parents=True, exist_ok=False)
    git(repository, "init", "-q", "-b", "main")
    for name, content in {
        "src/math.txt": "sum = a + b\n",
        "docs/guide.txt": "Guide version one\n",
        "notes.txt": "Unrelated original\n",
        "remove.txt": "Remove this later\n",
    }.items():
        path = repository / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
    git(repository, "add", ".")
    git(repository, "commit", "-qm", "initial fixture")
    (repository / "notes.txt").write_text("Unrelated staged change\n")
    git(repository, "add", "notes.txt")
    (repository / "notes.txt").write_text("Unrelated staged change\nUnstaged tail\n")
    (repository / "src/math.txt").write_text("sum = checked_add(a, b)\n")
    git(repository, "mv", "docs/guide.txt", "docs/manual.txt")
    (repository / "remove.txt").unlink()
    (repository / "new file.txt").write_text("Untracked public fixture\n")
    return repository


def snapshot(repository):
    return {
        "head": git(repository, "rev-parse", "HEAD"),
        "index": git(repository, "ls-files", "--stage"),
        "status": git(repository, "status", "--porcelain=v2", "--branch"),
        "files": {str(path.relative_to(repository)): path.read_bytes().hex()
                  for path in sorted(Path(repository).rglob("*"))
                  if path.is_file() and ".git" not in path.parts},
    }
