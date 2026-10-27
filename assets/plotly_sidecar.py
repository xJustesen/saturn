"""saturn plotly sidecar: reads NDJSON requests on stdin, writes NDJSON responses.

request:  {"id": int, "figure": <plotly figure json>, "width": px, "height": px, "scale": float}
response: {"id": int, "png_base64": str} | {"id": int, "error": str}

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
        task = asyncio.create_task(handle(renderer, out, req))
        tasks.add(task)
        task.add_done_callback(tasks.discard)
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)
    await renderer.close()


if __name__ == "__main__":
    asyncio.run(main())
