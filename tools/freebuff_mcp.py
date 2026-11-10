#!/usr/bin/env python3
"""MCP stdio bridge for the Freebuff CLI.

Freebuff has no headless mode (CodebuffAI/freebuff#1322 was closed), so this
drives its TUI through a pty. One Freebuff process is kept per session id;
tools can send follow-ups and close it. Output includes the visible screen,
the model actually selected, and the git diff produced in the working tree.

Dependencies: pexpect and pyte (`python3 -m pip install pexpect pyte`).
Everything is local; the only network traffic is Freebuff's own.
"""

import json
import os
import re
import subprocess
import sys
import threading
import time
import uuid

import pexpect
import pyte

WIDTH, HEIGHT = 120, 40
DEFAULT_MODEL = "DeepSeek V4.1 Flash Slow"
STARTUP_SECONDS = 8.0
PICKER_SECONDS = 4.0
MAX_SCREEN_LINES = 160
MAX_DIFF_BYTES = 60_000
MAX_REPORT_CHARS = 8_000


def log(message):
    print(f"[freebuff-mcp] {message}", file=sys.stderr, flush=True)


def strip_osc(raw):
    raw = re.sub(rb"\x1b\][^\x07]*\x07", b"", raw)
    return re.sub(rb"\x1b\][^\x1b]*\x1b\\", b"", raw)


def git(cwd, *args):
    try:
        return subprocess.run(
            ["git", *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout
    except Exception:
        return ""


class Session:
    def __init__(self, cwd, model):
        self.id = uuid.uuid4().hex[:12]
        self.cwd = os.path.abspath(cwd)
        self.model = model
        self.started = time.time()
        self.task_done = False
        self.task_result = None
        self.task_error = None
        self.progress_path = None
        self.screen = pyte.Screen(WIDTH, HEIGHT)
        self.stream = pyte.Stream(self.screen)
        self.stop = False
        self.child = pexpect.spawn(
            "freebuff",
            ["--cwd", self.cwd],
            env={**os.environ, "TERM": "xterm-256color"},
            dimensions=(HEIGHT, WIDTH),
            encoding=None,
            timeout=30,
        )
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()
        try:
            self.wait(STARTUP_SECONDS)
            self.select_model(model)
        except Exception:
            self.close()
            raise
        self.before_status = git(self.cwd, "status", "--porcelain=v1")
        self.before_diff = git(self.cwd, "diff", "--stat")

    def _read(self):
        while not self.stop:
            try:
                data = self.child.read_nonblocking(size=65536, timeout=0.1)
            except pexpect.TIMEOUT:
                continue
            except (pexpect.EOF, OSError):
                break
            if data:
                try:
                    self.stream.feed(strip_osc(data).decode("utf-8", "replace"))
                except Exception:
                    pass

    def wait(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            time.sleep(0.1)

    def text(self):
        return "\n".join(
            line.rstrip() for line in self.screen.display if line.strip()
        )

    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        self.child.send(data)

    def selected_model_line(self):
        for line in self.screen.display:
            if "›" in line:
                return line.strip()
        return ""

    def ready(self):
        deadline = time.time() + 20
        while time.time() < deadline:
            text = self.text().lower()
            if "enter a coding task" in text or "for commands" in text:
                return True
            self.wait(0.5)
        return False

    def open_picker(self):
        self.ready()
        self.send("/")
        self.wait(1.0)
        self.send("model")
        self.wait(0.5)
        self.send("\r")
        self.wait(PICKER_SECONDS)
        return self.text()

    def select_model(self, model):
        wanted = model.lower()
        for attempt in range(4):
            self.open_picker()
            deadline = time.time() + 8
            while time.time() < deadline:
                lines = self.screen.display
                if any(wanted in line.lower() for line in lines):
                    break
                self.wait(0.5)
            lines = self.screen.display
            target = next(
                (i for i, line in enumerate(lines) if wanted in line.lower()), None
            )
            if target is None:
                self.send("\x1b")
                self.wait(0.5)
                continue
            for _ in range(8):
                lines = self.screen.display
                selected = next(
                    (i for i, line in enumerate(lines) if "›" in line), target
                )
                if selected == target:
                    break
                self.send("\x1b[A" if target < selected else "\x1b[B")
                self.wait(0.12)
            self.send("\r")
            self.wait(2.0)
            footer = "\n".join(self.screen.display[-6:]).lower()
            if wanted in footer:
                self.model = model
                return
            self.send("\x1b")
            self.wait(0.5)
            log(
                f"model picker attempt {attempt + 1} failed; "
                f"selected={self.selected_model_line()!r}"
            )
        raise RuntimeError(
            f"Freebuff did not switch to {model!r}; screen:\n{self.text()}"
        )

    def ask(self, prompt, timeout):
        prompt = " ".join(prompt.splitlines())
        self.send(prompt)
        self.wait(0.4)
        self.send("\r")
        deadline = time.time() + timeout
        last = ""
        stable_since = time.time()
        started = time.time()
        while time.time() < deadline:
            self.wait(1.0)
            now = self.text()
            if now != last:
                last = now
                stable_since = time.time()
            if self.progress_path:
                try:
                    with open(self.progress_path, "w") as handle:
                        json.dump(
                            {
                                "session_id": self.id,
                                "model": self.model,
                                "elapsed": round(time.time() - started, 1),
                                "screen": "\n".join(now.splitlines()[-25:]),
                            },
                            handle,
                        )
                except OSError:
                    pass
            # An approval/choice prompt needs a human/agent decision.
            low = now.lower()
            if any(
                marker in low
                for marker in (
                    "allow this",
                    "allow the",
                    "do you want to",
                    "press enter to",
                    "needs your approval",
                )
            ):
                return {
                    "status": "needs_input",
                    "screen": now,
                    "elapsed": round(time.time() - started, 1),
                }
            # "Done" needs the input back, not a paused spinner. Freebuff can
            # sit on one screen for minutes while a slow model thinks.
            tail = "\n".join(now.splitlines()[-4:]).lower()
            busy = any(
                marker in tail
                for marker in ("working...", "thinking", "esc to interrupt", "running...")
            )
            if busy:
                stable_since = time.time()
            if (
                not busy
                and time.time() - started > 12
                and time.time() - stable_since > 10
            ):
                return {
                    "status": "done",
                    "screen": now,
                    "elapsed": round(time.time() - started, 1),
                }
        return {
            "status": "timeout",
            "screen": self.text(),
            "elapsed": round(time.time() - started, 1),
        }

    def diff(self):
        # Only files touched after the session started: the tree already had
        # uncommitted work, and returning all of it buries Freebuff's edits.
        raw = git(self.cwd, "status", "--porcelain=v1")
        touched = []
        for line in raw.splitlines():
            if not line.strip():
                continue
            path = line[3:].split(" -> ")[-1].strip()
            full = os.path.join(self.cwd, path)
            try:
                if os.path.getmtime(full) + 1 < self.started:
                    continue
            except OSError:
                pass
            touched.append(line)
        pieces = []
        for line in touched:
            path = line[3:].split(" -> ")[-1].strip()
            if line.startswith("??"):
                piece = git(self.cwd, "diff", "--no-index", "--", "/dev/null", path)
            else:
                piece = git(self.cwd, "diff", "--", path)
            if piece:
                pieces.append(piece)
        full = "\n".join(pieces)
        if len(full) > MAX_DIFF_BYTES:
            full = full[:MAX_DIFF_BYTES] + "\n...[diff truncated]"
        stat = "\n".join(
            line for line in git(self.cwd, "diff", "--stat").splitlines()
            if any(line.strip().startswith(os.path.basename(t[3:].split(" -> ")[-1].strip())) for t in touched)
        )
        return {"status": touched, "stat": stat, "diff": full}

    def close(self):
        self.stop = True
        try:
            self.child.sendcontrol("c")
            self.wait(0.4)
            self.child.sendcontrol("c")
        except Exception:
            pass
        try:
            self.child.close(force=True)
        except Exception:
            pass


SESSIONS = {}


def get_session(session_id):
    session = SESSIONS.get(session_id)
    if session is None:
        raise RuntimeError(f"unknown session {session_id!r}")
    return session


def run_checks(cwd, checks, timeout=300):
    rows = []
    for command in checks:
        started = time.time()
        try:
            proc = subprocess.run(
                command,
                cwd=cwd,
                shell=True,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            output = (proc.stdout + proc.stderr).strip().splitlines()
            rows.append(
                {
                    "command": command,
                    "ok": proc.returncode == 0,
                    "exit": proc.returncode,
                    "seconds": round(time.time() - started, 1),
                    "output": "\n".join(output[-25:]),
                }
            )
        except subprocess.TimeoutExpired:
            rows.append(
                {
                    "command": command,
                    "ok": False,
                    "exit": "timeout",
                    "seconds": round(time.time() - started, 1),
                    "output": "",
                }
            )
    return rows


def start_task(args):
    cwd = os.path.abspath(args.get("cwd") or os.getcwd())
    timeout = int(args.get("timeout_secs", 300))
    read_only = bool(args.get("read_only", False))
    checks = args.get("checks") or []
    if isinstance(checks, str):
        checks = [checks]
    report_path = os.path.join(
        "/tmp", f"freebuff-report-{uuid.uuid4().hex[:10]}.md"
    )
    rules = (
        "You are working in {cwd}. Do not commit, push, reset, rebase, force-push or "
        "delete files. Do not use interactive commands. "
    ).format(cwd=cwd)
    if read_only:
        rules += "Read-only task: do not edit or write any file in the repository. "
    else:
        rules += (
            "Make the changes directly and run the relevant tests. "
            "If a test is already failing for unrelated reasons, say so and do not "
            "weaken it. "
        )
    prompt = (
        rules
        + args["prompt"].strip()
        + "\n\nWhen finished, write a concise report to "
        + report_path
        + " containing: files changed (with one line each), commands run and their "
        "results, anything unfinished, and any concern. Do not paste large diffs."
    )
    session = Session(cwd, args.get("model") or DEFAULT_MODEL)
    session.progress_path = args.get("progress_path")
    SESSIONS[session.id] = session

    def worker():
        try:
            outcome = session.ask(prompt, timeout)
            checks_result = run_checks(cwd, checks) if checks else []
            report = ""
            try:
                with open(report_path) as handle:
                    report = handle.read().strip()
            except OSError:
                pass
            try:
                os.unlink(report_path)
            except OSError:
                pass
            session.task_result = {
                "session_id": session.id,
                "model": session.model,
                "status": outcome["status"],
                "cwd": cwd,
                "elapsed": outcome["elapsed"],
                "report": report[:MAX_REPORT_CHARS],
                "checks": checks_result,
                "diff": session.diff(),
                "screen": "\n".join(outcome["screen"].splitlines()[-40:]),
            }
        except Exception as error:
            session.task_error = str(error)
        finally:
            session.task_done = True

    threading.Thread(target=worker, daemon=True).start()
    return session


def session_task_result(session):
    if session.task_error:
        return {
            "session_id": session.id,
            "model": session.model,
            "status": "error",
            "cwd": session.cwd,
            "elapsed": round(time.time() - session.started, 1),
            "report": session.task_error,
            "checks": [],
            "diff": session.diff(),
            "screen": "",
        }
    if session.task_result is not None:
        return session.task_result
    return {
        "session_id": session.id,
        "model": session.model,
        "status": "running",
        "cwd": session.cwd,
        "elapsed": round(time.time() - session.started, 1),
        "report": "",
        "checks": [],
        "diff": session.diff(),
        "screen": "\n".join(session.text().splitlines()[-12:]),
    }


def wait_task(session, wait_secs):
    deadline = time.time() + max(0, wait_secs)
    while not session.task_done and time.time() < deadline:
        time.sleep(0.5)
    return session_task_result(session)


def task(args):
    session = start_task(args)
    result = wait_task(session, int(args.get("timeout_secs", 300)))
    if not args.get("keep"):
        if result["status"] == "running":
            result["status"] = "timeout"
        session.close()
        SESSIONS.pop(session.id, None)
        if result["status"] == "done":
            result["status"] = "closed"
    return result


def task_text(result):
    lines = [
        f"session: {result['session_id']}",
        f"model: {result['model']}",
        f"status: {result['status']}",
        f"cwd: {result['cwd']}",
        f"elapsed: {result['elapsed']}s",
    ]
    if result["report"]:
        lines += ["", "=== report ===", result["report"]]
    if result["checks"]:
        lines += ["", "=== checks ==="]
        for check in result["checks"]:
            lines.append(
                f"[{'PASS' if check['ok'] else 'FAIL'}] {check['command']} "
                f"({check['exit']}, {check['seconds']}s)"
            )
            if not check["ok"] and check["output"]:
                lines += ["  " + line for line in check["output"].splitlines()]
    lines += ["", "=== files ==="]
    lines += result["diff"]["status"] or ["(clean)"]
    if result["diff"]["diff"]:
        lines += ["", "=== diff ===", result["diff"]["diff"]]
    if not result["report"]:
        lines += ["", "=== screen tail ===", result["screen"]]
    return "\n".join(lines)


def tool_defs():
    return [
        {
            "name": "freebuff_task",
            "description": (
                "One-call Freebuff worker: select the model, run a self-contained "
                "task, independently run verification commands, read a concise "
                "report, and return only touched files + focused diff. This is the "
                "preferred tool for delegating code, review or verification work."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "Self-contained task."},
                    "cwd": {"type": "string"},
                    "model": {"type": "string", "default": DEFAULT_MODEL},
                    "checks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Shell commands run independently after Freebuff.",
                    },
                    "read_only": {"type": "boolean", "default": False},
                    "keep": {
                        "type": "boolean",
                        "default": False,
                        "description": "Keep the session open for follow-ups.",
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": 300,
                        "minimum": 30,
                        "maximum": 900,
                    },
                    "wait_secs": {
                        "type": "integer",
                        "default": 10,
                        "minimum": 0,
                        "maximum": 300,
                        "description": "How long this call waits before returning a running session.",
                    },
                    "progress_path": {
                        "type": "string",
                        "description": "Optional JSON file updated with the screen tail while running.",
                    },
                },
                "required": ["prompt"],
            },
        },
        {
            "name": "freebuff_sessions",
            "description": "List Freebuff task sessions and their state.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "freebuff_result",
            "description": (
                "Wait for a running freebuff_task and return its report, independent "
                "check results, touched files and focused diff."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"},
                    "wait_secs": {
                        "type": "integer",
                        "default": 60,
                        "minimum": 0,
                        "maximum": 300,
                    },
                },
                "required": ["session_id"],
            },
        },
        {
            "name": "freebuff_run",
            "description": (
                "Start a Freebuff coding session in a directory, select the model, "
                "send one task, and return the final screen plus git diff. "
                "Freebuff edits files directly; review the diff. Prefer freebuff_task. "
                "Use freebuff_followup for questions and freebuff_close when finished."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "Coding task."},
                    "cwd": {
                        "type": "string",
                        "description": "Working directory (default: current directory).",
                    },
                    "model": {
                        "type": "string",
                        "default": DEFAULT_MODEL,
                        "description": "Model row to select in /model.",
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "default": 240,
                        "minimum": 30,
                        "maximum": 900,
                    },
                },
                "required": ["prompt"],
            },
        },
        {
            "name": "freebuff_followup",
            "description": "Send a follow-up message to a running Freebuff session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"},
                    "prompt": {"type": "string"},
                    "timeout_secs": {
                        "type": "integer",
                        "default": 240,
                        "minimum": 30,
                        "maximum": 900,
                    },
                },
                "required": ["session_id", "prompt"],
            },
        },
        {
            "name": "freebuff_screen",
            "description": "Return the current screen and diff of a session.",
            "inputSchema": {
                "type": "object",
                "properties": {"session_id": {"type": "string"}},
                "required": ["session_id"],
            },
        },
        {
            "name": "freebuff_close",
            "description": "Close a Freebuff session and return its final diff.",
            "inputSchema": {
                "type": "object",
                "properties": {"session_id": {"type": "string"}},
                "required": ["session_id"],
            },
        },
    ]


def result_text(result):
    parts = [
        f"session: {result['session_id']}",
        f"model: {result['model']}",
        f"status: {result['status']}",
        f"cwd: {result['cwd']}",
        "",
        "=== git status ===",
        "\n".join(result["diff"]["status"]) or "(clean)",
        "=== git diff --stat ===",
        result["diff"]["stat"] or "(none)",
    ]
    if result["diff"]["diff"]:
        parts += ["", "=== git diff ===", result["diff"]["diff"]]
    parts += ["", "=== screen ===", "\n".join(result["screen"].splitlines()[-MAX_SCREEN_LINES:])]
    return "\n".join(parts)


def call_tool(name, args):
    if name == "freebuff_task":
        session = start_task(args)
        result = wait_task(session, int(args.get("wait_secs", 10)))
        result["kind"] = "task"
        return result
    if name == "freebuff_sessions":
        return {
            "sessions": [
                {
                    "session_id": session.id,
                    "model": session.model,
                    "status": (
                        "done"
                        if session.task_done
                        else "running"
                    ),
                    "elapsed": round(time.time() - session.started, 1),
                    "cwd": session.cwd,
                }
                for session in SESSIONS.values()
            ]
        }
    if name == "freebuff_result":
        session = get_session(args["session_id"])
        result = wait_task(session, int(args.get("wait_secs", 60)))
        result["kind"] = "task"
        return result
    if name == "freebuff_run":
        cwd = args.get("cwd") or os.getcwd()
        model = args.get("model") or DEFAULT_MODEL
        timeout = int(args.get("timeout_secs", 240))
        session = None
        try:
            session = Session(cwd, model)
        except Exception:
            if session is not None:
                session.close()
            raise
        SESSIONS[session.id] = session
        outcome = session.ask(args["prompt"], timeout)
        return {
            "session_id": session.id,
            "model": session.model,
            "cwd": session.cwd,
            **outcome,
            "diff": session.diff(),
        }
    if name == "freebuff_followup":
        session = get_session(args["session_id"])
        outcome = session.ask(args["prompt"], int(args.get("timeout_secs", 240)))
        return {
            "session_id": session.id,
            "model": session.model,
            "cwd": session.cwd,
            **outcome,
            "diff": session.diff(),
        }
    if name == "freebuff_screen":
        session = get_session(args["session_id"])
        return {
            "session_id": session.id,
            "model": session.model,
            "status": "running",
            "cwd": session.cwd,
            "screen": session.text(),
            "diff": session.diff(),
        }
    if name == "freebuff_close":
        session = get_session(args["session_id"])
        diff = session.diff()
        session.close()
        del SESSIONS[session.id]
        return {
            "session_id": session.id,
            "model": session.model,
            "status": "closed",
            "cwd": session.cwd,
            "screen": "",
            "diff": diff,
        }
    raise RuntimeError(f"unknown tool {name!r}")


def reply(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    log("ready")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except Exception:
            continue
        method = message.get("method")
        msg_id = message.get("id")
        if method == "initialize":
            reply(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "protocolVersion": message.get("params", {}).get(
                            "protocolVersion", "2024-11-05"
                        ),
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "freebuff", "version": "1.0.0"},
                    },
                }
            )
        elif method in ("notifications/initialized", "notifications/cancelled"):
            continue
        elif method == "ping":
            reply({"jsonrpc": "2.0", "id": msg_id, "result": {}})
        elif method == "tools/list":
            reply({"jsonrpc": "2.0", "id": msg_id, "result": {"tools": tool_defs()}})
        elif method == "tools/call":
            params = message.get("params", {})
            name = params.get("name")
            args = params.get("arguments", {})
            try:
                result = call_tool(name, args)
                text = (
                    task_text(result)
                    if result.get("kind") == "task"
                    else result_text(result)
                )
                reply(
                    {
                        "jsonrpc": "2.0",
                        "id": msg_id,
                        "result": {
                            "content": [{"type": "text", "text": text}],
                            "isError": False,
                        },
                    }
                )
            except Exception as error:
                log(f"{name}: {error}")
                reply(
                    {
                        "jsonrpc": "2.0",
                        "id": msg_id,
                        "result": {
                            "content": [{"type": "text", "text": str(error)}],
                            "isError": True,
                        },
                    }
                )
        elif msg_id is not None:
            reply(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {"code": -32601, "message": f"unknown method {method}"},
                }
            )
    for session in list(SESSIONS.values()):
        session.close()


def cli():
    import argparse

    parser = argparse.ArgumentParser(description="Run a one-shot Freebuff task.")
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run")
    run.add_argument("prompt")
    run.add_argument("--cwd", default=os.getcwd())
    run.add_argument("--model", default=DEFAULT_MODEL)
    run.add_argument("--check", action="append", default=[])
    run.add_argument("--read-only", action="store_true")
    run.add_argument("--keep", action="store_true")
    run.add_argument("--json", action="store_true")
    run.add_argument("--timeout", type=int, default=300)
    run.add_argument("--progress")
    args = parser.parse_args()
    if args.command == "run":
        result = task(
            {
                "prompt": args.prompt,
                "cwd": args.cwd,
                "model": args.model,
                "checks": args.check,
                "read_only": args.read_only,
                "keep": args.keep,
                "timeout_secs": args.timeout,
                "progress_path": args.progress,
            }
        )
        print(json.dumps(result, indent=2) if args.json else task_text(result))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "run":
        cli()
    else:
        main()
