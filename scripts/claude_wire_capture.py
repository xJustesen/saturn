#!/usr/bin/env python3
"""Record the claude CLI's stream-json wire traffic for saturn's AI client.

Spawns the real, logged-in `claude` the way saturn does and writes every JSON
line in both directions, with the time since spawn, to
`<out>/<scenario>.jsonl` as `{"t": seconds, "dir": "out"|"in", "msg": {...}}`
(`out` is the CLI's stdout, `in` is what the host wrote to its stdin).

    scripts/claude_wire_capture.py <out-dir> [scenario ...]

This spends (few) subscription tokens. Re-run it when the CLI's control
protocol may have changed, then pass the result through `--sanitize` before
committing it under `tests/fixtures/ai/`:

    scripts/claude_wire_capture.py --sanitize <raw-dir> tests/fixtures/ai
"""

import base64
import json
import os
import queue
import re
import struct
import subprocess
import sys
import threading
import time
import zlib

MCP_CONFIG = {"mcpServers": {"saturn": {"type": "sdk", "name": "saturn"}}}

DUMMY_TOOL = {
    "name": "dummy",
    "description": "Echo the given text back, together with a small test image.",
    "inputSchema": {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
    },
}


def png(rgb=(255, 0, 0), size=16):
    """A solid-colour PNG, built by hand so the script needs no packages."""

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    row = b"\x00" + bytes(rgb) * size
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(row * size))
        + chunk(b"IEND", b"")
    )


def b64png(rgb):
    return base64.b64encode(png(rgb)).decode()


def clean_env():
    """Saturn runs from a plain terminal: no host-injected Claude variables
    and never an API key (a key always wins over the subscription login)."""
    return {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "ANTHROPIC_"))}


class Session:
    def __init__(self, out_dir, name, extra_args=(), model="haiku", decide=None, env=None, init=True):
        self.decide = decide or (lambda req: {"behavior": "allow", "updatedInput": req.get("input", {})})
        self.next_id = 0
        env = {**clean_env(), **(env or {})}
        args = [
            os.environ.get("SATURN_CLAUDE_BIN", "claude"),
            "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--permission-prompt-tool", "stdio",
            "--strict-mcp-config",
            "--mcp-config", json.dumps(MCP_CONFIG),
            "--no-session-persistence",
            *(["--model", model] if model else []),
            *extra_args,
        ]  # fmt: skip
        cwd = os.path.join(out_dir, "cwd")
        os.makedirs(cwd, exist_ok=True)
        self.log = open(os.path.join(out_dir, f"{name}.jsonl"), "w")
        self.t0 = time.monotonic()
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(out_dir, f"{name}.stderr"), "w"),
            env=env,
            cwd=cwd,
            text=True,
            bufsize=1,
        )
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        if init:
            # What saturn's client does first: the SDK handshake. Its reply is
            # the only sign of life before the first user message.
            rid = self.control({"subtype": "initialize", "hooks": None})
            self.pump(lambda m: m.get("type") == "control_response" and m["response"].get("request_id") == rid, 60)

    def _read(self):
        for line in self.proc.stdout:
            t = time.monotonic() - self.t0
            try:
                msg = json.loads(line)
            except ValueError:
                msg = {"_raw": line.rstrip("\n")}
            self.lines.put((t, msg))
        self.lines.put((time.monotonic() - self.t0, None))

    def _record(self, t, direction, msg):
        self.log.write(json.dumps({"t": round(t, 3), "dir": direction, "msg": msg}) + "\n")
        self.log.flush()

    def send(self, msg):
        self._record(time.monotonic() - self.t0, "in", msg)
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def user(self, *blocks):
        content = [{"type": "text", "text": b} if isinstance(b, str) else b for b in blocks]
        self.send({"type": "user", "message": {"role": "user", "content": content}, "parent_tool_use_id": None})

    def control(self, request):
        self.next_id += 1
        request_id = f"saturn_{self.next_id}"
        self.send({"type": "control_request", "request_id": request_id, "request": request})
        return request_id

    def respond(self, request_id, response):
        self.send({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": response},
        })  # fmt: skip

    def pump(self, until, timeout=120.0, on_msg=None):
        """Read (and answer) messages until `until(msg)`; returns that message."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                t, msg = self.lines.get(timeout=max(0.0, deadline - time.monotonic()))
            except queue.Empty:
                return None
            if msg is None:
                self._record(t, "out", {"_eof": True})
                return None
            self._record(t, "out", msg)
            if msg.get("type") == "control_request":
                self._answer(msg)
            if on_msg:
                on_msg(msg)
            if until(msg):
                return msg

    def _answer(self, msg):
        req = msg.get("request", {})
        if req.get("subtype") == "mcp_message":
            self.respond(msg["request_id"], {"mcp_response": mcp_reply(req.get("message", {}))})
        elif req.get("subtype") == "can_use_tool":
            decision = self.decide(req)
            if decision is not None:  # None leaves the prompt pending
                self.respond(msg["request_id"], decision)
        # Anything else is left unanswered so it shows up in the capture.

    def turn(self, *blocks, **kw):
        self.user(*blocks)
        return self.pump(lambda m: m.get("type") == "result", **kw)

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        self.pump(lambda m: False, timeout=10)
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self._record(time.monotonic() - self.t0, "in", {"_exit": self.proc.returncode})


def mcp_reply(rpc):
    """The in-band `saturn` MCP server: one dummy tool."""
    method, rpc_id = rpc.get("method"), rpc.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": rpc.get("params", {}).get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "saturn", "version": "0.0.0"},
        }
    elif method == "tools/list":
        result = {"tools": [DUMMY_TOOL]}
    elif method == "tools/call":
        text = rpc.get("params", {}).get("arguments", {}).get("text", "")
        result = {
            "content": [
                {"type": "text", "text": f"dummy says: {text}"},
                {"type": "image", "data": b64png((0, 0, 255)), "mimeType": "image/png"},
            ],
            "isError": False,
        }
    elif rpc_id is None:
        return {"jsonrpc": "2.0", "result": {}}  # a notification
    else:
        return {"jsonrpc": "2.0", "id": rpc_id, "error": {"code": -32601, "message": f"no method {method}"}}
    return {"jsonrpc": "2.0", "id": rpc_id, "result": result}


def is_init(m):
    return m.get("type") == "system" and m.get("subtype") == "init"


def scenario_idle(out):
    """Does anything arrive before the host writes a byte?"""
    s = Session(out, "idle", init=False)
    s.pump(lambda m: False, timeout=8)
    s.close()


def scenario_basic(out):
    """Init handshake, two text turns in one process, then an image turn."""
    s = Session(out, "basic")
    s.turn("Reply with exactly: hello there")
    s.turn("Now reply with exactly: bye now")
    s.turn(
        "What colour is this image? Answer with one word.",
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": b64png((255, 0, 0))}},
    )
    s.close()


def scenario_noinit(out):
    """A turn without the `initialize` control request."""
    s = Session(out, "noinit", init=False)
    s.turn("Reply with exactly: ok")
    s.close()


def scenario_permission(out):
    """A built-in tool that needs permission: allowed once, denied once."""

    def decide(req):
        if "deny" in json.dumps(req.get("input")):
            return {"behavior": "deny", "message": "The user declined this command."}
        return {"behavior": "allow", "updatedInput": req.get("input", {})}

    s = Session(out, "permission", decide=decide)
    s.turn("Use the Bash tool to run `touch saturn_allow.txt`. Then reply with only: done")
    s.turn("Use the Bash tool to run `touch saturn_deny.txt`. If it is refused, reply with only: refused")
    s.close()


def scenario_mcp(out):
    """An in-band MCP tool call that returns text and an image."""
    s = Session(out, "mcp")
    s.turn(
        "Call the mcp__saturn__dummy tool with text \"ping\". "
        "Then reply with the text it returned and the colour of the image it returned, nothing else."
    )
    s.close()


def scenario_interrupt(out):
    """Interrupt mid-stream, switch model, then prove the process still works."""
    s = Session(out, "interrupt")
    s.user("Count from 1 to 300, one number per line, no other text.")
    seen = {"deltas": 0, "sent": False}

    def on_msg(m):
        if m.get("type") == "stream_event" and m["event"].get("type") == "content_block_delta":
            seen["deltas"] += 1
            if seen["deltas"] == 3 and not seen["sent"]:
                seen["sent"] = True
                s.control({"subtype": "interrupt"})

    s.pump(lambda m: m.get("type") == "result", on_msg=on_msg)
    rid = s.control({"subtype": "set_model", "model": "sonnet"})
    s.pump(lambda m: m.get("type") == "control_response" and m["response"].get("request_id") == rid, timeout=30)
    s.turn("Reply with exactly: still here")
    s.close()


def scenario_always(out):
    """"Always allow": the reply carries a session rule, so the same call does
    not prompt a second time."""

    def decide(req):
        rules = [
            {**sug, "destination": "session"}
            for sug in req.get("permission_suggestions", [])
            if sug.get("type") == "addRules"
        ]
        return {"behavior": "allow", "updatedInput": req.get("input", {}), "updatedPermissions": rules}

    s = Session(out, "always", decide=decide)
    s.turn("Use the Bash tool to run exactly `touch saturn_always.txt`. Then reply with only: done")
    s.turn("Use the Bash tool to run exactly `touch saturn_always.txt` again. Then reply with only: done")
    s.close()


def scenario_interrupt_pending(out):
    """Interrupt while a permission prompt is still unanswered."""
    s = Session(out, "interrupt_pending", decide=lambda req: None)
    s.user("Use the Bash tool to run `touch saturn_pending.txt`. Then reply with only: done")
    s.pump(lambda m: m.get("type") == "control_request" and m["request"].get("subtype") == "can_use_tool")
    s.control({"subtype": "interrupt"})
    s.pump(lambda m: m.get("type") == "result", timeout=30)
    s.turn("Reply with exactly: still here")
    s.close()


def scenario_default(out):
    """The account's default model and a host-chosen session id."""
    s = Session(out, "default", model=None, extra_args=["--session-id", "5a7e4b1c-0000-4000-8000-000000000001"])
    s.turn("Reply with exactly: hello there")
    s.turn("Now reply with exactly: bye now")
    s.close()


def scenario_loggedout(out):
    """No login: an empty config directory hides the stored credentials."""
    cfg = os.path.join(out, "empty-config")
    os.makedirs(cfg, exist_ok=True)
    env = {**clean_env(), "CLAUDE_CONFIG_DIR": cfg}
    r = subprocess.run(["claude", "auth", "status", "--json"], capture_output=True, text=True, env=env)
    with open(os.path.join(out, "auth_status_loggedout.json"), "w") as f:
        f.write(r.stdout)
    print(f"logged-out auth status: exit {r.returncode} stderr={r.stderr[:200]!r}")
    s = Session(out, "loggedout", env={"CLAUDE_CONFIG_DIR": cfg})
    s.turn("Reply with exactly: ok", timeout=30)
    s.close()


SCENARIOS = {
    "idle": scenario_idle,
    "basic": scenario_basic,
    "noinit": scenario_noinit,
    "permission": scenario_permission,
    "mcp": scenario_mcp,
    "interrupt": scenario_interrupt,
    "always": scenario_always,
    "interrupt_pending": scenario_interrupt_pending,
    "default": scenario_default,
    "loggedout": scenario_loggedout,
}


def auth_status(out):
    t = time.monotonic()
    r = subprocess.run(["claude", "auth", "status", "--json"], capture_output=True, text=True, env=clean_env())
    with open(os.path.join(out, "auth_status.json"), "w") as f:
        f.write(r.stdout)
    print(f"auth status: exit {r.returncode} in {time.monotonic() - t:.2f}s")


# --- sanitising -----------------------------------------------------------

# Lists in `system/init` (and the `initialize` reply) that describe the
# recording machine's own setup; the parser does not read them.
EMPTY_LISTS = {"slash_commands", "skills", "plugins", "agents", "commands"}
REDACT_KEYS = {"email", "orgId", "orgName", "organization", "userId", "accountUuid", "organizationUuid"}


def sanitize_value(v, subs):
    if isinstance(v, dict):
        out = {}
        for k, x in v.items():
            if k in REDACT_KEYS:
                out[k] = "redacted" if isinstance(x, str) else None
            elif k in EMPTY_LISTS and isinstance(x, list):
                out[k] = []
            else:
                out[k] = sanitize_value(x, subs)
        return out
    if isinstance(v, list):
        return [sanitize_value(i, subs) for i in v]
    if isinstance(v, str):
        for pattern, repl in subs:
            v = pattern.sub(repl, v)
    return v


def sanitize(src, dst):
    home = os.path.expanduser("~")
    user = os.path.basename(home)
    slug = re.sub(r"[^A-Za-z0-9]", "-", user)
    subs = [
        # The capture's working directory, wherever the temp dir put it.
        (re.compile(r"/[^\s\"']*?/cwd(?=[/\s\"']|$)"), "/Users/user/project"),
        (re.compile(r"-[A-Za-z0-9-]*-cwd\b"), "-Users-user-project"),
        (re.compile(r"/[^\s\"']*?/empty-config"), "/Users/user/.claude"),
        (re.compile(re.escape(home)), "/Users/user"),
        (re.compile(re.escape(user)), "user"),
        (re.compile(re.escape(slug)), "user"),
    ]
    os.makedirs(dst, exist_ok=True)
    for name in sorted(os.listdir(src)):
        path = os.path.join(src, name)
        if name.endswith(".jsonl"):
            with open(path) as f, open(os.path.join(dst, name), "w") as g:
                for line in f:
                    g.write(json.dumps(sanitize_value(json.loads(line), subs), separators=(",", ":")) + "\n")
        elif name.endswith(".json"):
            with open(path) as f, open(os.path.join(dst, name), "w") as g:
                json.dump(sanitize_value(json.load(f), subs), g, indent=2)
                g.write("\n")


def main():
    args = sys.argv[1:]
    if args and args[0] == "--sanitize":
        sanitize(args[1], args[2])
        return
    if not args:
        sys.exit(__doc__)
    out = args[0]
    os.makedirs(out, exist_ok=True)
    for name in args[1:] or [*SCENARIOS, "auth"]:
        if name == "auth":
            auth_status(out)
            continue
        t = time.monotonic()
        SCENARIOS[name](out)
        print(f"{name}: {time.monotonic() - t:.1f}s")


if __name__ == "__main__":
    main()
