#!/usr/bin/env python3
"""A stand-in for the `claude` CLI that replays a scripted stream-json session.

Point saturn at it with `SATURN_CLAUDE_BIN=scripts/fake_claude.py`: no real
CLI, no login and no tokens are involved.

The session is a JSONL file, one step per line, played in order:

    {"dir": "out", "msg": {...}}     write the message to stdout
    {"dir": "in", "msg": {...}}      wait for a stdin line of the same kind
    {"dir": "in", "expect": {...}}   wait for a stdin line containing `expect`
    {"sleep": 0.05}                  pause
    {"mark": true} / {"loop": true}  at `loop`, continue after `mark`

A step with `"when": "mcp"` is skipped unless saturn passed `--mcp-config`.
The recordings in `tests/fixtures/ai/` are sessions in this format (their `t`
is ignored). A stdin line that does not fit ends the process with exit code 2
and a message on stderr.

The session file is `$FAKE_CLAUDE_SESSION`, else `fake_claude_session.jsonl`
in the working directory, else the demo session next to this script. With a
session file in the working directory the fake also records what it was given:
its arguments and environment in `fake_claude_argv.json` and every stdin line
in `fake_claude_stdin.jsonl`.

`fake_claude.py auth status --json` answers like the real one; a file named
`fake_claude_logged_out` in the working directory makes it say "not logged in".
"""

import json
import os
import sys
import time

LOCAL_SESSION = "fake_claude_session.jsonl"


def auth_status():
    logged_in = not os.path.exists("fake_claude_logged_out")
    status = {"loggedIn": logged_in, "authMethod": "claude.ai" if logged_in else "none", "apiProvider": "firstParty"}
    print(json.dumps(status, indent=2))
    sys.exit(0 if logged_in else 1)


def kind(msg):
    """What a recorded host message must have in common with the live one."""
    t = msg.get("type")
    if t == "control_request":
        return (t, msg.get("request", {}).get("subtype"))
    if t == "control_response":
        response = msg.get("response", {})
        return (t, response.get("subtype"), response.get("request_id"))
    return (t,)


def contains(got, expect):
    if isinstance(expect, dict):
        return isinstance(got, dict) and all(k in got and contains(got[k], v) for k, v in expect.items())
    if isinstance(expect, list):
        return isinstance(got, list) and len(got) == len(expect) and all(map(contains, got, expect))
    return got == expect


def fail(n, why):
    print(f"fake_claude: step {n}: {why}", file=sys.stderr, flush=True)
    sys.exit(2)


def main():
    argv = sys.argv[1:]
    if argv[:2] == ["auth", "status"]:
        auth_status()

    record = os.path.exists(LOCAL_SESSION)
    path = os.environ.get("FAKE_CLAUDE_SESSION") or (
        LOCAL_SESSION if record else os.path.join(os.path.dirname(os.path.abspath(__file__)), LOCAL_SESSION)
    )
    with open(path) as f:
        steps = [json.loads(line) for line in f if line.strip()]
    stdin_log = None
    if record:
        with open("fake_claude_argv.json", "w") as f:
            env = {k: k in os.environ for k in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN")}
            json.dump({"argv": argv, "env": env, "cwd": os.getcwd()}, f)
        stdin_log = open("fake_claude_stdin.jsonl", "w")

    has_mcp = "--mcp-config" in argv
    mark = 0
    exit_code = 0
    n = 0
    while n < len(steps):
        step = steps[n]
        n += 1
        if step.get("when") == "mcp" and not has_mcp:
            continue
        if "sleep" in step:
            time.sleep(step["sleep"])
        elif step.get("mark"):
            mark = n
        elif step.get("loop"):
            n = mark
        elif "_exit" in step.get("msg", {}):
            exit_code = step["msg"]["_exit"] or 0
        elif "_eof" in step.get("msg", {}):
            break
        elif step.get("dir") == "out":
            sys.stdout.write(json.dumps(step["msg"], separators=(",", ":")) + "\n")
            sys.stdout.flush()
        elif step.get("dir") == "in":
            line = sys.stdin.readline()
            if not line:
                sys.exit(0)  # the host hung up: a normal shutdown
            if stdin_log:
                stdin_log.write(line)
                stdin_log.flush()
            try:
                got = json.loads(line)
            except ValueError:
                fail(n, f"stdin is not JSON: {line!r}")
            if "expect" in step:
                if not contains(got, step["expect"]):
                    fail(n, f"expected {json.dumps(step['expect'])}, got {line.strip()}")
            elif kind(got) != kind(step["msg"]):
                fail(n, f"expected {kind(step['msg'])}, got {kind(got)}: {line.strip()[:300]}")
        else:
            fail(n, f"unknown step {json.dumps(step)[:200]}")

    # Like the real CLI: stay until the host closes stdin.
    for line in sys.stdin:
        if stdin_log:
            stdin_log.write(line)
            stdin_log.flush()
    for step in steps[n:]:
        if "_exit" in step.get("msg", {}):
            exit_code = step["msg"]["_exit"] or 0
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
