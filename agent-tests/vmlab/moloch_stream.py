#!/usr/bin/env python3
"""Mirror an eval run into Moloch as a watchable agent run.

The vmlab eval is a batch of `(task, model)` runs. Piped to a log, that batch
is invisible while it runs and fragile if one model dies. Moloch already knows
how to *watch* agent work — spawn a run, stream its transcript, settle it — and
the app renders that live in its Runs list. So the eval becomes Moloch's first
real worker: each run is spawned, claimed, streamed, and settled here, and you
watch it in the app instead of tailing a buffer.

This module is just the client for Moloch's existing worker protocol
(`moloch/src/lib.rs`): no server changes. One run is four calls —

    spawn    POST /api/v1/agent/run                        -> run_id (QUEUED)
    claim    POST /api/v1/worker/poll                      -> job_id
    stream   POST /api/v1/agent/runs/{id}/transcript-append   (first flips it
                                                              to INPROGRESS)
    settle   POST /api/v1/worker/jobs/{job}/complete       -> SUCCEEDED/FAILED

Config:
    MOLOCH_URL     base URL           (default http://127.0.0.1:10102)
    MOLOCH_TOKEN   bearer token       (default: ~/.moloch/api-token)
    MOLOCH_WORKER  worker name shown  (default vmlab-eval)

Prove the loop against a live local Moloch, no PC and no guest:
    python3 moloch_stream.py --selftest
"""

import json
import os
import pathlib
import time
import urllib.error
import urllib.request

MOLOCH_URL = os.environ.get("MOLOCH_URL", "http://127.0.0.1:10102").rstrip("/")
WORKER = os.environ.get("MOLOCH_WORKER", "vmlab-eval")


def _token():
    token = os.environ.get("MOLOCH_TOKEN")
    if token and token.strip():
        return token.strip()
    path = pathlib.Path.home() / ".moloch" / "api-token"
    return path.read_text().strip() if path.exists() else ""


TOKEN = _token()


def _req(method, path, body=None, timeout=30):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        MOLOCH_URL + path,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {TOKEN}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        raw = response.read()
        return json.loads(raw) if raw else {}


class MolochUnavailable(RuntimeError):
    """Moloch could not be reached or refused the request. The eval treats this
    as non-fatal — the grade is what matters — so a run still completes locally
    even when nobody is watching."""


class MolochRun:
    """One eval run, mirrored into Moloch as a watchable agent run.

    Deliberately best-effort: every method swallows transport errors into a
    disabled state rather than raising, so wrapping the eval in one of these
    can never turn "Moloch is down" into "the eval crashed". The `.ok` flag
    says whether the mirror is live.
    """

    def __init__(self, title, model, prompt):
        self.title = title
        self.model = model
        self.run_id = None
        self.job_id = None
        self.ok = False
        self._started = False
        try:
            resp = _req(
                "POST",
                "/api/v1/agent/run",
                {"prompt": prompt, "title": title, "config": {"model_id": model}},
            )
            self.run_id = resp["run_id"]
            self.job_id = self._claim()
            self.ok = True
        except (urllib.error.URLError, OSError, KeyError, ValueError) as e:
            self.error = f"{type(e).__name__}: {e}"

    def _claim(self):
        # claim() hands back the oldest queued agent_run job (it doesn't filter
        # on model). During a sweep the eval owns this Moloch, so the job we
        # just enqueued is the one we get — but assert it by run_id, and if a
        # stray older job surfaces, settle it rather than stream this run's
        # transcript onto the wrong record.
        for _ in range(10):
            resp = _req(
                "POST",
                "/api/v1/worker/poll",
                {"worker": WORKER, "kinds": "agent_run", "models": self.model},
            )
            job = resp.get("job")
            if not job:
                time.sleep(0.2)
                continue
            run = resp.get("run") or {}
            if self.run_id in (run.get("task_id"), run.get("run_id")):
                return job["id"]
            # A job that isn't ours — put a floor under it and keep polling.
            try:
                _req(
                    "POST",
                    f"/api/v1/worker/jobs/{job['id']}/complete",
                    {"ok": False, "error": "drained by vmlab-eval (foreign job)"},
                )
            except (urllib.error.URLError, OSError):
                pass
        raise MolochUnavailable("could not claim the run's own job")

    def append(self, lines, event=None):
        """Stream transcript lines (a string is split on newlines). The first
        call flips the run to INPROGRESS via a run_started event, so it lights
        up in the app the moment work begins."""
        if not self.ok:
            return
        if isinstance(lines, str):
            lines = lines.splitlines()
        body = {"lines": list(lines)}
        if not self._started:
            body["event"] = event or "run_started"
            self._started = True
        elif event:
            body["event"] = event
        try:
            _req("POST", f"/api/v1/agent/runs/{self.run_id}/transcript-append", body)
        except (urllib.error.URLError, OSError):
            self.ok = False

    def complete(self, passed, summary, error=None):
        """Settle the run: SUCCEEDED when passed, FAILED otherwise. `summary`
        becomes the run's result_summary, which the app's list shows."""
        if not self.ok or self.job_id is None:
            return
        try:
            _req(
                "POST",
                f"/api/v1/worker/jobs/{self.job_id}/complete",
                {"ok": bool(passed), "output": {"summary": summary}, "error": error},
            )
        except (urllib.error.URLError, OSError):
            self.ok = False


def _selftest():
    if not TOKEN:
        raise SystemExit("no token — set MOLOCH_TOKEN or create ~/.moloch/api-token")
    print(f"streaming a demo run into {MOLOCH_URL} …")
    run = MolochRun(
        "vmlab:deploy-user (selftest)",
        "qwen2.5-coder:7b",
        "Create a system user 'deploy' in the www-data group with an ssh key.",
    )
    if not run.ok:
        raise SystemExit(f"could not start the mirror: {getattr(run, 'error', '?')}")
    print(f"  run_id={run.run_id} job_id={run.job_id}")
    run.append(["asking qwen2.5-coder:7b for a plan…"])
    time.sleep(0.4)
    run.append(
        [
            "$ useradd -m -G www-data deploy",
            "$ install -d -m700 -o deploy -g deploy ~deploy/.ssh",
            "$ install -m600 -o deploy key.pub ~deploy/.ssh/authorized_keys",
        ]
    )
    time.sleep(0.4)
    run.append(
        [
            "PASS deploy exists and is in www-data",
            "PASS ~/.ssh is 700, owned by deploy",
            "PASS authorized_keys holds the key, mode 600",
        ],
        event="grade",
    )
    run.complete(True, "3/3 checks passed")
    print(f"  settled SUCCEEDED — open the app's Moloch page (Runs), or:")
    print(f"    curl -s -H 'Authorization: Bearer <tok>' {MOLOCH_URL}/api/v1/agent/runs/{run.run_id} | jq .state")
    print(f"    curl -s -H 'Authorization: Bearer <tok>' {MOLOCH_URL}/api/v1/agent/runs/{run.run_id}/transcript")


if __name__ == "__main__":
    import sys

    if sys.argv[1:2] == ["--selftest"]:
        _selftest()
    else:
        raise SystemExit(__doc__)
