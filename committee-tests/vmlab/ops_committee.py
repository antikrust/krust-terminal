#!/usr/bin/env python3
"""Point local models at a sysadmin task, run their plans in the vmlab guest,
and grade the guest's end state — the committee, for sysadmin.

    ops_committee.py <task-dir>...        # one or more tasks/<name>/
    ops_committee.py --demo               # a built-in task, to prove the loop

Per task, per model: reset the guest to pristine, ask the model for a plan
(shell commands only), run the whole plan inside the guest, then run the
task's held-out check.sh against the guest's end state. Reset again. Nothing
a model writes runs anywhere but the disposable guest.

The split that made the code committee work applies here unchanged: the model
proposes, the machine checks. A model is good at proposing commands and bad at
judging whether they worked — so the grade is never the model's opinion, it is
check.sh reading the real system by value.

Environment:
    VMLAB        path to vmlab.sh          (default /mnt/terra/vmlab/vmlab.sh)
    OLLAMA_HOST  http://127.0.0.1:11434
    MODELS       comma-separated tags      (default qwen2.5-coder:7b)
"""

import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.request

VMLAB = os.environ.get("VMLAB", "/mnt/terra/vmlab/vmlab.sh")
HERE = pathlib.Path(__file__).resolve().parent
RUNGUEST = str(HERE / "runguest.sh")
HOST = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")
MODELS = [m.strip() for m in os.environ.get("MODELS", "qwen2.5-coder:7b").split(",") if m.strip()]


def _commit():
    # When run from a deploy dir that isn't a git checkout (the PC), the caller
    # passes the real revision in OPS_COMMIT; git is the fallback for local runs.
    if os.environ.get("OPS_COMMIT"):
        return os.environ["OPS_COMMIT"]
    try:
        out = subprocess.run(
            ["git", "-C", str(HERE), "rev-parse", "--short", "HEAD"],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return out.stdout.strip() or "unknown" if out.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


COMMIT = _commit()

PLAN_SYSTEM = (
    "You are a careful Linux sysadmin working at a root shell on Ubuntu 22.04 "
    "(systemd, apt). Given a task, output ONLY the shell commands that accomplish "
    "it — one command per line, no prose, no markdown fences, no comments, no "
    "explanation. Use sudo where needed. The machine is a fresh server image, so "
    "install anything you rely on with `apt-get install -y` and prefer "
    "non-interactive flags (DEBIAN_FRONTEND=noninteractive). Do not reboot."
)


def ollama(model, system, user):
    body = json.dumps(
        {
            "model": model,
            "stream": False,
            "options": {"temperature": 0},
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        }
    ).encode()
    req = urllib.request.Request(
        f"{HOST}/api/chat", body, {"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=900) as response:
        return json.load(response)["message"]["content"]


def vmlab(*args, stdin=None):
    return subprocess.run(
        [VMLAB, *args], input=stdin, capture_output=True, text=True, timeout=900
    )


def clean_plan(text):
    """A model's reply as an executable script: drop fences, prose and blanks,
    keep the command lines. Kept generous — a stray prose line will simply
    fail in the guest, which the grade catches, rather than being silently
    dropped and hiding what the model actually did."""
    lines = []
    for raw in text.splitlines():
        line = raw.rstrip()
        stripped = line.strip()
        if not stripped or stripped.startswith("```") or stripped.startswith("#"):
            continue
        lines.append(line)
    return "\n".join(lines)


def grade(task_dir):
    check = task_dir / "check.sh"
    if not check.exists():
        return None, "no check.sh"
    result = subprocess.run(
        ["sh", str(check), RUNGUEST],
        capture_output=True,
        text=True,
        timeout=900,
        env={**os.environ, "VMLAB": VMLAB},
    )
    return result.returncode == 0, result.stdout.strip()


def run_one(task_dir, model):
    prompt = (task_dir / "PROMPT.txt").read_text().strip()
    vmlab("reset")

    precheck = task_dir / "precheck.sh"
    if precheck.exists():
        pre = subprocess.run(
            ["sh", str(precheck), RUNGUEST],
            capture_output=True,
            text=True,
            env={**os.environ, "VMLAB": VMLAB},
        )
        if pre.returncode == 0:
            print(f"  ! precheck already satisfied on a pristine guest — task is trivial")

    started = time.time()
    reply = ollama(model, PLAN_SYSTEM, prompt)
    plan = clean_plan(reply)
    # Stage the plan as a file and run it with stdin from /dev/null. Piping the
    # plan straight into `bash -s` looks equivalent but is a trap: an
    # interactive command in the plan (adduser asking for a full name, apt
    # asking to confirm) reads from the same stdin and swallows the *following*
    # commands as its answers. That silently dropped a `usermod -aG sudo` once
    # and the grade caught it — the plan was right, the execution ate it.
    #
    # The env that keeps apt non-interactive has to be set INSIDE the script:
    # sudo scrubs the environment, so a DEBIAN_FRONTEND on the outer command is
    # gone by the time apt runs, and Ubuntu 22.04's needrestart then pops a
    # dialog that wedges the whole plan (it once ran for ten minutes). And
    # `timeout` bounds a genuinely stuck plan — a foregrounded daemon, a slow
    # mirror — into a clean failure the grade can read instead of a wall-clock
    # hang.
    preamble = "export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a NEEDRESTART_SUSPEND=1\n"
    vmlab("run", "cat > /tmp/ops_plan.sh", stdin=preamble + plan + "\n")
    execution = vmlab("run", "sudo timeout 420 bash /tmp/ops_plan.sh </dev/null")
    # Some end states only exist after time passes — a minutely timer has to
    # actually fire once before its log line is there to grade. A task that
    # needs this carries a `settle` file with the seconds to wait; everything
    # else grades immediately.
    settle = task_dir / "settle"
    if settle.exists():
        try:
            time.sleep(int(settle.read_text().strip()))
        except ValueError:
            pass
    passed, report = grade(task_dir)
    seconds = int(time.time() - started)

    verdict = "PASS" if passed else "FAIL"
    # One greppable line per run, stamped with the commit that produced it —
    # an ops score is a fact about this pipeline at this revision, same as the
    # code bench's BENCH line, so a table of them can be told when it is stale.
    print(
        f"OPSBENCH task={task_dir.name} model={model} passed={int(bool(passed))} "
        f"seconds={seconds} commit={COMMIT}"
    )
    print(f"  {verdict}  {task_dir.name}  [{model}]  {seconds}s")
    for line in report.splitlines():
        print(f"       {line}")
    if not passed:
        # The plan and what the guest said back — the only way to see whether
        # the model planned it wrong or the commands failed.
        print(f"       --- plan ---")
        for line in plan.splitlines():
            print(f"       $ {line}")
        if execution.stderr.strip():
            tail = execution.stderr.strip().splitlines()[-4:]
            print(f"       --- stderr tail ---")
            for line in tail:
                print(f"       {line}")
    return {
        "task": task_dir.name,
        "model": model,
        "passed": bool(passed),
        "seconds": seconds,
        "plan": plan,
        "report": report,
    }


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)

    if args == ["--demo"]:
        demo = HERE / "tasks" / "_demo"
        (demo).mkdir(parents=True, exist_ok=True)
        (demo / "PROMPT.txt").write_text(
            "Create a system user called 'deploy' that is a member of the 'sudo' "
            "group and whose login shell is /bin/bash.\n"
        )
        # Every check goes through "$RUN" so it reads the guest, never the host.
        (demo / "check.sh").write_text(
            "#!/bin/sh\n"
            'RUN="$1"; fails=0\n'
            '"$RUN" \'id deploy\' >/dev/null 2>&1 && echo "PASS deploy exists" || { echo "FAIL no deploy user"; fails=1; }\n'
            '"$RUN" \'id -nG deploy\' 2>/dev/null | grep -qw sudo && echo "PASS in sudo group" || { echo "FAIL not in sudo"; fails=1; }\n'
            '[ "$("$RUN" \'getent passwd deploy | cut -d: -f7\' 2>/dev/null)" = /bin/bash ] && echo "PASS shell is bash" || { echo "FAIL wrong shell"; fails=1; }\n'
            'test "$fails" -eq 0 && echo "all good" || echo "$fails failing"\n'
            'exit "$fails"\n'
        )
        task_dirs = [demo]
    else:
        task_dirs = [pathlib.Path(a).resolve() for a in args]

    results = []
    for task_dir in task_dirs:
        if not (task_dir / "PROMPT.txt").exists():
            print(f"skip {task_dir}: no PROMPT.txt")
            continue
        print(f"=== {task_dir.name} ===")
        for model in MODELS:
            results.append(run_one(task_dir, model))

    passed = sum(1 for r in results if r["passed"])
    print(f"\nOPS {passed}/{len(results)} passed")


if __name__ == "__main__":
    main()
