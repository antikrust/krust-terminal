#!/usr/bin/env python3
"""Watch ONE (task, model) run happen, live, in Moloch.

Where ops_bench.py sweeps and grades many runs, this stares at a single one:
it streams the model *writing* its plan (and its thinking, for models that
emit any), then the plan running in the guest, then each held-out check — all
into the run's Moloch transcript, so you watch it unfold in the web panel
instead of reading a post-mortem.

    watch_run.py <task-dir> <model>
    watch_run.py tasks/heartbeat-timer qwen2.5-coder:14b

Needs MOLOCH_URL/MOLOCH_TOKEN (the mirror), VMLAB (the guest), OLLAMA_HOST.
"""

import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

import moloch_stream

VMLAB = os.environ.get("VMLAB", "/mnt/terra/vmlab/vmlab.sh")
OLLAMA = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")
HERE = pathlib.Path(__file__).resolve().parent
RUNGUEST = str(HERE / "runguest.sh")
try:
    os.chmod(RUNGUEST, 0o755)
except OSError:
    pass

PLAN_SYSTEM = (
    "You are a careful Linux sysadmin working at a root shell on Ubuntu 22.04 "
    "(systemd, apt). Given a task, output ONLY the shell commands that accomplish "
    "it — one command per line, no prose, no markdown fences, no comments, no "
    "explanation. Use sudo where needed. The machine is a fresh server image, so "
    "install anything you rely on with `apt-get install -y` and prefer "
    "non-interactive flags (DEBIAN_FRONTEND=noninteractive). Do not reboot."
)


def vmlab(*args, stdin=None):
    return subprocess.run(
        [VMLAB, *args], input=stdin, capture_output=True, text=True, timeout=900
    )


def clean_plan(text):
    lines = []
    for raw in text.splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("```") or stripped.startswith("#"):
            continue
        lines.append(raw.rstrip())
    return "\n".join(lines)


def _chat_stream(model, system, user, think):
    payload = {
        "model": model,
        "stream": True,
        "options": {"temperature": 0},
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    }
    if think:
        payload["think"] = True
    req = urllib.request.Request(
        f"{OLLAMA}/api/chat", json.dumps(payload).encode(), {"Content-Type": "application/json"}
    )
    return urllib.request.urlopen(req, timeout=900)


def stream_generation(model, system, user, mirror):
    """Stream the model's reply, flushing whole lines into the transcript as
    they form — thinking prefixed with a marker, plan lines indented. Returns
    the full content. `think: true` asks Ollama for a thinking channel; a model
    without one answers 400, so fall back to a plain stream."""
    try:
        resp = _chat_stream(model, system, user, True)
    except urllib.error.HTTPError as e:
        if e.code != 400:
            raise
        resp = _chat_stream(model, system, user, False)
    content, cbuf, tbuf = [], "", ""
    with resp:
        for raw in resp:
            raw = raw.strip()
            if not raw:
                continue
            try:
                msg = json.loads(raw)
            except ValueError:
                continue
            message = msg.get("message", {})
            think = message.get("thinking")
            if think:
                tbuf += think
                while "\n" in tbuf:
                    line, tbuf = tbuf.split("\n", 1)
                    if line.strip():
                        mirror.append([f"  \U0001f4ad {line.strip()}"])
            piece = message.get("content", "")
            if piece:
                content.append(piece)
                cbuf += piece
                while "\n" in cbuf:
                    line, cbuf = cbuf.split("\n", 1)
                    if line.strip():
                        mirror.append([f"    {line.rstrip()}"])
            if msg.get("done"):
                break
    if tbuf.strip():
        mirror.append([f"  \U0001f4ad {tbuf.strip()}"])
    if cbuf.strip():
        mirror.append([f"    {cbuf.rstrip()}"])
    return "".join(content)


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    task_dir = pathlib.Path(sys.argv[1]).resolve()
    model = sys.argv[2]
    prompt = (task_dir / "PROMPT.txt").read_text().strip()

    # `watch:` not `vmlab:` on purpose — a one-off stare shouldn't count toward
    # the benchmark leaderboard (which aggregates only `vmlab:%` runs).
    mirror = moloch_stream.MolochRun(f"watch:{task_dir.name}", model, prompt)
    if not mirror.ok:
        raise SystemExit(f"could not open a Moloch run: {getattr(mirror, 'error', '?')}")
    print(f"run {mirror.run_id} — watch it in the Moloch web panel")

    vmlab("reset")
    mirror.append(["── the job ──", prompt, f"── {model} is writing a plan ──"])

    try:
        reply = stream_generation(model, PLAN_SYSTEM, prompt, mirror)
    except Exception as e:
        mirror.append([f"model error: {e}"])
        mirror.complete(False, "model error", str(e))
        raise SystemExit(f"model error: {e}")
    plan = clean_plan(reply)

    mirror.append(["── running the plan in the guest ──"], event="exec")
    preamble = "export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a NEEDRESTART_SUSPEND=1\n"
    vmlab("run", "cat > /tmp/ops_plan.sh", stdin=preamble + plan + "\n")
    execution = vmlab("run", "sudo timeout 420 bash /tmp/ops_plan.sh </dev/null")
    out = (execution.stdout + execution.stderr).strip()
    mirror.append([f"  {line}" for line in out.splitlines()[-40:]] or ["  (no output)"])

    settle = task_dir / "settle"
    if settle.exists():
        try:
            secs = int(settle.read_text().strip())
            mirror.append([f"── waiting {secs}s for the effect to actually happen ──"])
            time.sleep(secs)
        except ValueError:
            pass

    mirror.append(["── grading the machine it left behind ──"], event="grade")
    result = subprocess.run(
        ["sh", str(task_dir / "check.sh"), RUNGUEST],
        capture_output=True,
        text=True,
        timeout=900,
        env={**os.environ, "VMLAB": VMLAB},
    )
    passed = result.returncode == 0
    report = result.stdout.strip()
    mirror.append([f"  {line}" for line in report.splitlines()] or ["  (no grader output)"])
    summary = report.splitlines()[-1].strip() if report else ("passed" if passed else "failed")
    mirror.complete(passed, summary, None if passed else report[:800])
    print(("PASS" if passed else "FAIL"), task_dir.name, model)


if __name__ == "__main__":
    main()
