"""saturn plotly sidecar: reads NDJSON requests on stdin, writes NDJSON responses.

request:  {"id": int, "figure": <plotly figure json>, "width": px, "height": px, "scale": float}
response: {"id": int, "png_base64": str} | {"id": int, "error": str}
"""
import base64
import json
import sys


def main() -> None:
    try:
        import plotly.io as pio  # noqa: F401
    except Exception as e:  # pragma: no cover
        sys.stdout.write(json.dumps({"id": 0, "error": f"plotly import failed: {e}"}) + "\n")
        sys.stdout.flush()
        return
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            rid = req.get("id", 0)
        except Exception as e:
            sys.stdout.write(json.dumps({"id": 0, "error": f"bad request: {e}"}) + "\n")
            sys.stdout.flush()
            continue
        try:
            fig = pio.from_json(json.dumps(req["figure"]), skip_invalid=True)
            png = pio.to_image(
                fig,
                format="png",
                width=int(req.get("width") or 800),
                height=int(req.get("height") or 500),
                scale=float(req.get("scale") or 1.0),
            )
            resp = {"id": rid, "png_base64": base64.b64encode(png).decode("ascii")}
        except Exception as e:
            resp = {"id": rid, "error": f"{type(e).__name__}: {e}"[:400]}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
