"""saturn plotly sidecar: reads NDJSON requests on stdin, writes NDJSON responses.

request:  {"id": int, "figure": <plotly figure json>, "width": px, "height": px, "scale": float}
response: {"id": int, "png_base64": str} | {"id": int, "error": str}

Live (interactive) figures use the same pipes. A session is a browser tab
showing the figure with plotly.js; mouse input is replayed into it and a
screenshot is sent back whenever the page changes:

request:  {"live": "open", "sid": int, "figure": ..., "width": px, "height": px}
          {"live": "input", "sid": int, "type": "move"|"down"|"up"|"wheel",
           "x": px, "y": px, "button": "left"|"middle"|"right"|"none", "dx": px, "dy": px}
          {"live": "close", "sid": int}
response: {"live": sid, "png_base64": str} | {"live": sid, "error": str}

One headless Chrome (a `kaleido.Kaleido` instance) is started up front and kept
for the life of the process: `plotly.io.to_image` would launch a fresh browser
for every figure, which costs more than a second each. With the browser warm a
figure renders in tens of milliseconds. Requests are rendered concurrently (the
browser queues them) and answered as they finish, so responses may arrive out
of order; the caller matches them by id.
"""
import asyncio
import base64
import json
import sys
import threading
import time

TABS = 1  # Chrome tabs; more render in parallel but take longer to start.


def emit(lock: threading.Lock, obj: dict) -> None:
    with lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


class Renderer:
    """A kaleido instance, restarted if the browser goes away."""

    def __init__(self) -> None:
        import kaleido

        self.kaleido = kaleido
        self.inst = None
        self.lock = asyncio.Lock()

    async def start(self) -> None:
        async with self.lock:
            if self.inst is None:
                inst = self.kaleido.Kaleido(n=TABS)
                await inst.__aenter__()
                self.inst = inst

    async def restart(self) -> None:
        async with self.lock:
            old, self.inst = self.inst, None
            if old is not None:
                try:
                    await old.__aexit__(None, None, None)
                except Exception:
                    pass
        await self.start()

    async def close(self) -> None:
        async with self.lock:
            inst, self.inst = self.inst, None
        if inst is not None:
            try:
                await inst.__aexit__(None, None, None)
            except Exception:
                pass

    async def render(self, fig: dict, opts: dict) -> bytes:
        from kaleido.errors import BrowserClosedError, BrowserFailedError

        await self.start()
        try:
            return await self.inst.calc_fig(fig, opts=opts)
        except (BrowserClosedError, BrowserFailedError):
            await self.restart()
            return await self.inst.calc_fig(fig, opts=opts)


async def handle(renderer: Renderer, out: threading.Lock, req: dict) -> None:
    rid = req.get("id", 0)
    try:
        opts = dict(
            format="png",
            width=int(req.get("width") or 800),
            height=int(req.get("height") or 500),
            scale=float(req.get("scale") or 1.0),
        )
        png = await renderer.render(req["figure"], opts)
        resp = {"id": rid, "png_base64": base64.b64encode(png).decode("ascii")}
    except Exception as e:
        resp = {"id": rid, "error": f"{type(e).__name__}: {e}"[:400]}
    emit(out, resp)


LIVE_JS = """(async (fig, w, h) => {
  document.body.style.margin = '0';
  document.body.style.overflow = 'hidden';
  document.body.innerHTML = '<div id="g"></div>';
  const layout = Object.assign({}, fig.layout || {}, {width: w, height: h, autosize: false});
  const config = Object.assign({}, fig.config || {}, {responsive: false, scrollZoom: true, displaylogo: false});
  await Plotly.newPlot('g', fig.data || [], layout, config);
  if (fig.frames && fig.frames.length) await Plotly.addFrames('g', fig.frames);
  return true;
})"""

SETTLE = 0.04  # seconds between screenshots while the page may still be changing
QUIET_SHOTS = 4  # unchanged screenshots in a row before a session goes idle
DOUBLE_CLICK = 0.4  # seconds


def cdp_ok(resp: dict) -> dict:
    if "error" in resp:
        raise RuntimeError(str(resp["error"])[:300])
    return resp.get("result", {})


class LiveSession:
    """One figure in its own tab, driven by forwarded mouse input."""

    def __init__(self, renderer: "Renderer", out: threading.Lock, sid: int) -> None:
        self.renderer = renderer
        self.out = out
        self.sid = sid
        self.queue: asyncio.Queue = asyncio.Queue()
        self.tab = None
        self.held = "none"
        self.last_down = (0.0, -1e9, -1e9, "none")
        self.clicks = 1
        self.last_frame = None

    async def run(self, req: dict) -> None:
        try:
            await self.open(req)
            await self.loop()
        except Exception as e:
            emit(self.out, {"live": self.sid, "error": f"{type(e).__name__}: {e}"[:400]})
        finally:
            await self.close_tab()

    async def open(self, req: dict) -> None:
        from choreographer.protocol.devtools_async_helpers import create_and_wait

        await self.renderer.start()
        browser = self.renderer.inst
        w = max(int(req.get("width") or 800), 50)
        h = max(int(req.get("height") or 500), 50)
        self.tab = await create_and_wait(browser, browser._index)
        cdp_ok(
            await self.tab.send_command(
                "Emulation.setDeviceMetricsOverride",
                {"width": w, "height": h, "deviceScaleFactor": 1, "mobile": False},
            )
        )
        expr = f"{LIVE_JS}({json.dumps(req['figure'])}, {w}, {h})"
        res = cdp_ok(
            await self.tab.send_command(
                "Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}
            )
        )
        if "exceptionDetails" in res:
            detail = res["exceptionDetails"]
            text = detail.get("exception", {}).get("description") or detail.get("text") or "plotly error"
            raise RuntimeError(text.splitlines()[0])

    async def loop(self) -> None:
        quiet = 0
        while True:
            if quiet >= QUIET_SHOTS:
                ev = await self.queue.get()
            else:
                try:
                    ev = await asyncio.wait_for(self.queue.get(), SETTLE)
                except asyncio.TimeoutError:
                    ev = False
            while ev is not False:
                if ev is None:
                    return
                # Only the last of a run of moves matters.
                if not (ev.get("type") == "move" and self.next_is_move()):
                    await self.dispatch(ev)
                quiet = 0
                ev = self.queue.get_nowait() if not self.queue.empty() else False
            if await self.shoot():
                quiet = 0
            else:
                quiet += 1

    def next_is_move(self) -> bool:
        q = self.queue._queue  # peek; asyncio.Queue has no public way
        return bool(q) and q[0] is not None and q[0].get("type") == "move"

    async def dispatch(self, ev: dict) -> None:
        kind = ev.get("type")
        x, y = float(ev.get("x") or 0), float(ev.get("y") or 0)
        button = ev.get("button") or "none"
        masks = {"left": 1, "right": 2, "middle": 4, "none": 0}
        params = {"x": x, "y": y}
        if kind == "move":
            params.update(type="mouseMoved", button=self.held, buttons=masks.get(self.held, 0))
        elif kind == "down":
            now = time.monotonic()
            t, lx, ly, lb = self.last_down
            double = now - t < DOUBLE_CLICK and lb == button and abs(lx - x) < 6 and abs(ly - y) < 6
            self.clicks = 2 if double and self.clicks == 1 else 1
            self.last_down = (now, x, y, button)
            self.held = button
            params.update(type="mousePressed", button=button, buttons=masks.get(button, 0), clickCount=self.clicks)
        elif kind == "up":
            self.held = "none"
            params.update(type="mouseReleased", button=button, buttons=0, clickCount=self.clicks)
        elif kind == "wheel":
            params.update(
                type="mouseWheel", deltaX=float(ev.get("dx") or 0), deltaY=float(ev.get("dy") or 0)
            )
        else:
            return
        cdp_ok(await self.tab.send_command("Input.dispatchMouseEvent", params))

    async def shoot(self) -> bool:
        """Screenshot the tab; send it if it differs from the last one sent."""
        res = cdp_ok(
            await self.tab.send_command("Page.captureScreenshot", {"format": "png", "optimizeForSpeed": True})
        )
        data = res.get("data")
        if not data or data == self.last_frame:
            return False
        self.last_frame = data
        emit(self.out, {"live": self.sid, "png_base64": data})
        return True

    async def close_tab(self) -> None:
        tab, self.tab = self.tab, None
        if tab is not None:
            try:
                await self.renderer.inst.close_tab(tab.target_id)
            except Exception:
                pass


def handle_live(renderer: Renderer, out: threading.Lock, sessions: dict, tasks: set, req: dict) -> None:
    sid = req.get("sid", 0)
    op = req.get("live")
    if op == "open":
        session = LiveSession(renderer, out, sid)
        sessions[sid] = session
        task = asyncio.create_task(session.run(req))
        tasks.add(task)
        task.add_done_callback(tasks.discard)
        task.add_done_callback(lambda _t: sessions.pop(sid, None) if sessions.get(sid) is session else None)
    elif sid in sessions:
        sessions[sid].queue.put_nowait(None if op == "close" else req)


async def main() -> None:
    out = threading.Lock()
    try:
        import plotly  # noqa: F401  (kaleido takes plotly.js from the plotly package)

        renderer = Renderer()
        await renderer.start()
    except Exception as e:
        emit(out, {"id": 0, "error": f"plotly/kaleido startup failed: {e}"})
        return
    emit(out, {"id": 0, "ready": True})

    loop = asyncio.get_running_loop()
    queue: asyncio.Queue = asyncio.Queue()

    def pump() -> None:
        for line in sys.stdin:
            loop.call_soon_threadsafe(queue.put_nowait, line)
        loop.call_soon_threadsafe(queue.put_nowait, None)

    threading.Thread(target=pump, daemon=True).start()

    tasks: set = set()
    sessions: dict = {}
    while True:
        line = await queue.get()
        if line is None:
            break
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception as e:
            emit(out, {"id": 0, "error": f"bad request: {e}"})
            continue
        if "live" in req:
            handle_live(renderer, out, sessions, tasks, req)
            continue
        task = asyncio.create_task(handle(renderer, out, req))
        tasks.add(task)
        task.add_done_callback(tasks.discard)
    for session in sessions.values():
        session.queue.put_nowait(None)
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)
    await renderer.close()


if __name__ == "__main__":
    asyncio.run(main())
