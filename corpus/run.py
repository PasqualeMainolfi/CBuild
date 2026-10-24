#!/usr/bin/env python3
"""Measures cbuild on the real projects listed in corpus.toml.

For each project: check out the pinned commit, try cbuild's heuristics alone
(`cbuild build --no-ai`), then, if they fail, the AI build engineer
(`cbuild build --yes`). The resulting plan is checked against the expected
targets. Projects are compiled, never run.

Usage:
    python3 -I corpus/run.py                    # whole corpus
    python3 -I corpus/run.py --only kilo,lua    # some projects
    python3 -I corpus/run.py --no-ai            # heuristics only
    python3 -I corpus/run.py --provider codex   # AI through Codex instead of the default
"""

import argparse
import datetime
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CORPUS = REPO / "corpus" / "corpus.toml"
RESULTS = REPO / "corpus" / "results"


def run(cmd, cwd, timeout):
    """Runs `cmd`, killing its whole process group (Claude Code included) on timeout."""
    started = time.monotonic()
    env = dict(os.environ, NO_COLOR="1")
    proc = subprocess.Popen(
        cmd, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        start_new_session=True,
    )
    try:
        output, _ = proc.communicate(timeout=timeout)
        timed_out = False
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        output, _ = proc.communicate()
        timed_out = True
    return {
        "ok": proc.returncode == 0 and not timed_out,
        "timed_out": timed_out,
        "seconds": round(time.monotonic() - started, 1),
        "output": output,
    }


def checkout(project, workdir):
    """A clean checkout of the pinned commit: no cbuild.toml, no previous outputs."""
    path = workdir / project["name"]
    git = ["git", "-c", "advice.detachedHead=false"]
    if not (path / ".git").exists():
        shutil.rmtree(path, ignore_errors=True)
        path.mkdir(parents=True)
        subprocess.run(git + ["init", "-q"], cwd=path, check=True)
        subprocess.run(git + ["remote", "add", "origin", project["repo"]], cwd=path, check=True)
    subprocess.run(git + ["fetch", "-q", "--depth", "1", "origin", project["rev"]], cwd=path, check=True)
    subprocess.run(git + ["checkout", "-q", "-f", "FETCH_HEAD"], cwd=path, check=True)
    subprocess.run(git + ["clean", "-q", "-fdx"], cwd=path, check=True)
    return path


def session_stats(path):
    """Tool calls made in the project's AI sessions, from their traces."""
    calls, notes = {}, ""
    for session in sorted((path / "target" / "cbuild-agent" / "sessions").glob("*")):
        trace = session / "trace.jsonl"
        if trace.exists():
            for line in trace.read_text().splitlines():
                tool = json.loads(line)["tool"]
                calls[tool] = calls.get(tool, 0) + 1
        result = session / "result.json"
        if result.exists():
            notes = json.loads(result.read_text()).get("notes", "")
    return calls, notes


def check_plan(project, path):
    """Which expected targets the saved or inferred plan provides."""
    manifest = path / "cbuild.toml"
    if manifest.exists():
        targets = tomllib.loads(manifest.read_text()).get("targets", [])
    else:
        plan = subprocess.run(
            [str(REPO / "target/release/cbuild"), "plan"], cwd=path,
            capture_output=True, text=True,
        )
        targets = tomllib.loads(plan.stdout).get("targets", []) if plan.returncode == 0 else []
    executables = {t["name"].lower() for t in targets if t.get("kind") == "executable"}
    missing = [e for e in project.get("executables", []) if e.lower() not in executables]
    if project.get("library") and not any(t.get("kind") == "static_library" for t in targets):
        missing.append("(a library)")
    return [t["name"] for t in targets], missing


def last_lines(text, n=4):
    lines = [l for l in text.strip().splitlines() if l.strip()]
    return " / ".join(lines[-n:])[:400]


def measure(project, cbuild, workdir, use_ai, timeout):
    path = checkout(project, workdir)
    result = {"name": project["name"], "difficulty": project["difficulty"]}

    heuristics = run([cbuild, "build", "--no-ai"], path, timeout)
    result["heuristics"] = {k: heuristics[k] for k in ("ok", "seconds")}
    stage, attempt = ("heuristics", heuristics) if heuristics["ok"] else (None, heuristics)

    if not heuristics["ok"] and use_ai:
        attempt = run([cbuild, "build", "--yes"], path, timeout)
        calls, notes = session_stats(path)
        result["ai"] = {
            "ok": attempt["ok"], "seconds": attempt["seconds"], "timed_out": attempt["timed_out"],
            "build_attempts": calls.get("try_build", 0), "tool_calls": sum(calls.values()),
            "notes": notes,
        }
        stage = "ai" if attempt["ok"] else None

    result["stage"] = stage or "failed"
    if stage:
        result["targets"], result["missing"] = check_plan(project, path)
    else:
        result["error"] = last_lines(attempt["output"])
    return result


def report(results, label):
    total = len(results)
    by_stage = {s: sum(r["stage"] == s for r in results) for s in ("heuristics", "ai", "failed")}
    complete = sum(r["stage"] != "failed" and not r.get("missing") for r in results)
    lines = [
        f"# cbuild corpus — {label}",
        "",
        f"- Built: **{total - by_stage['failed']}/{total}** "
        f"(heuristics {by_stage['heuristics']}, AI {by_stage['ai']}), failed {by_stage['failed']}",
        f"- Built with every expected target: **{complete}/{total}**",
        "",
        "| Project | Level | Result | Expected targets | Build attempts | Time | Notes |",
        "|---|---|---|---|---|---|---|",
    ]
    for r in results:
        ai = r.get("ai", {})
        outcome = {"heuristics": "✅ heuristics", "ai": "✅ AI", "failed": "❌"}[r["stage"]]
        if r["stage"] == "failed":
            expected = "—"
        elif r["missing"]:
            expected = "missing " + ", ".join(r["missing"])
        else:
            expected = "all"
        seconds = r["heuristics"]["seconds"] + ai.get("seconds", 0)
        notes = r.get("error") if r["stage"] == "failed" else ai.get("notes", "")
        notes = (notes or "").replace("|", "\\|").replace("\n", " ")
        lines.append(
            f"| {r['name']} | {r['difficulty']} | {outcome} | {expected} | "
            f"{ai.get('build_attempts', '—')} | {seconds:.0f}s | {notes} |"
        )
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma-separated project names")
    parser.add_argument("--no-ai", action="store_true", help="measure the heuristics only")
    parser.add_argument("--provider", choices=["claude-code", "codex"], help="AI provider (default: cbuild's choice)")
    parser.add_argument("--workdir", default=str(REPO / "target" / "corpus"), help="where projects are checked out")
    parser.add_argument("--timeout", type=int, default=1200, help="seconds per cbuild run")
    args = parser.parse_args()

    projects = tomllib.loads(CORPUS.read_text())["project"]
    if args.only:
        wanted = set(args.only.split(","))
        projects = [p for p in projects if p["name"] in wanted]

    subprocess.run(["cargo", "build", "--release", "-q"], cwd=REPO, check=True)
    cbuild = str(REPO / "target" / "release" / "cbuild")
    workdir = Path(args.workdir)

    results = []
    for project in projects:
        print(f"--- {project['name']} ({project['difficulty']})", file=sys.stderr, flush=True)
        try:
            result = measure(project, cbuild, workdir, not args.no_ai, args.timeout)
        except subprocess.CalledProcessError as err:
            result = {"name": project["name"], "difficulty": project["difficulty"], "stage": "failed",
                      "heuristics": {"ok": False, "seconds": 0}, "error": f"checkout failed: {err}"}
        print(f"    {result['stage']} {result.get('missing') or ''}", file=sys.stderr, flush=True)
        results.append(result)

    if args.provider:
        os.environ["CBUILD_AI_PROVIDER"] = args.provider
    # cbuild's default: Claude Code if installed, else Codex.
    provider = args.provider or ("claude-code" if shutil.which("claude") else "codex")
    label = datetime.date.today().isoformat() + ("-heuristics" if args.no_ai else f"-{provider}")
    full = RESULTS / f"{label}.json"
    if args.only and full.exists():
        # Rerunning some projects updates their rows in the day's full report.
        fresh = {r["name"]: r for r in results}
        results = [fresh.pop(r["name"], r) for r in json.loads(full.read_text())] + list(fresh.values())
    elif args.only:
        label += "-partial"
    RESULTS.mkdir(parents=True, exist_ok=True)
    (RESULTS / f"{label}.json").write_text(json.dumps(results, indent=2) + "\n")
    markdown = report(results, label)
    (RESULTS / f"{label}.md").write_text(markdown)
    print(markdown)


if __name__ == "__main__":
    main()
