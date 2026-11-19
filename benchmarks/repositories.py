"""Measure status backends and local fetch with hyperfine on isolated repositories."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gita", type=Path)
    parser.add_argument("--hyperfine", default="hyperfine")
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    exe = str(args.gita.resolve())
    with tempfile.TemporaryDirectory(prefix="gita-bench-") as tmp:
        root = Path(tmp)
        remote = root / "origin.git"
        subprocess.run(["git", "init", "--bare", "-q", remote], check=True)
        seed = root / "seed"
        subprocess.run(["git", "clone", "-q", remote, seed], check=True)
        subprocess.run(["git", "-C", seed, "-c", "user.name=Bench", "-c",
                        "user.email=bench@example.invalid", "-c", "commit.gpgsign=false",
                        "commit", "-qm", "baseline", "--allow-empty"], check=True)
        subprocess.run(["git", "-C", seed, "push", "-q", "origin", "HEAD"], check=True)
        config = root / "gita"
        config.mkdir()
        repos = {}
        env = dict(os.environ, GITA_PROJECT_HOME=tmp)
        for size in (10, 100, 500):
            for index in range(len(repos), size):
                name = f"repo-{index:03}"
                path = root / name
                shutil.copytree(seed, path)
                repos[name] = {"path": str(path)}
            (config / "repos.json").write_text(json.dumps({"version": 1, "repos": repos}))
            for backend, command in (("gix", "ll -C"), ("git", "ll -C"), ("auto", "fetch")):
                print(f"{size} repositories, {backend}, {command}", flush=True)
                subprocess.run([args.hyperfine, "--shell=none", "--runs", str(args.runs),
                                f'"{exe}" {command}'], env=dict(env, GITA_BACKEND=backend), check=True)


if __name__ == "__main__":
    main()
