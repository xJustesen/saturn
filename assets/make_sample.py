"""Generate assets/sample.ipynb with pre-computed outputs (matplotlib PNG,
plotly JSON, pandas HTML, an error, streams, markdown)."""
import base64
import io
import json
import sys
import uuid

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import plotly.express as px


def cell(kind, source, outputs=None, count=None):
    c = {
        "id": uuid.uuid4().hex[:8],
        "cell_type": kind,
        "metadata": {},
        "source": source.splitlines(keepends=True),
    }
    if kind == "code":
        c["execution_count"] = count
        c["outputs"] = outputs or []
    return c


def stream(name, text):
    return {"output_type": "stream", "name": name, "text": text.splitlines(keepends=True)}


def exec_result(count, data):
    return {"output_type": "execute_result", "execution_count": count, "data": data, "metadata": {}}


def display(data):
    return {"output_type": "display_data", "data": data, "metadata": {}}


x = np.linspace(0, 4 * np.pi, 400)
fig, ax = plt.subplots(figsize=(6.4, 3.6))
ax.plot(x, np.sin(x), label="sin")
ax.plot(x, np.cos(x), label="cos")
ax.legend()
ax.set_title("matplotlib figure")
buf = io.BytesIO()
fig.savefig(buf, format="png", dpi=100)
png_b64 = base64.b64encode(buf.getvalue()).decode()

pfig = px.line(x=x[::10], y=np.sin(x[::10]), title="plotly figure")
plotly_json = json.loads(pfig.to_json())

df = pd.DataFrame({"a": [1, 2, 3], "b": [4.5, 5.5, 6.5], "name": ["x", "y", "z"]})

cells = [
    cell("markdown", "# saturn sample notebook\n\nThis notebook exercises **markdown**, code, streams, errors and *figures*.\n\n- item one\n- item two\n\n```python\nprint('code block')\n```\n"),
    cell("code", "import numpy as np\nimport pandas as pd\nimport matplotlib.pyplot as plt\nprint('hello from python')\n", [stream("stdout", "hello from python\n")], 1),
    cell("code", "df = pd.DataFrame({'a': [1, 2, 3], 'b': [4.5, 5.5, 6.5], 'name': ['x', 'y', 'z']})\ndf\n",
         [exec_result(2, {"text/plain": df.to_string().splitlines(keepends=True), "text/html": df.to_html().splitlines(keepends=True)})], 2),
    cell("code", "x = np.linspace(0, 4 * np.pi, 400)\nfig, ax = plt.subplots(figsize=(6.4, 3.6))\nax.plot(x, np.sin(x), label='sin')\nax.plot(x, np.cos(x), label='cos')\nax.legend()\nax.set_title('matplotlib figure')\nplt.show()\n",
         [display({"image/png": png_b64, "text/plain": ["<Figure size 640x360 with 1 Axes>"]})], 3),
    cell("code", "import plotly.express as px\npx.line(x=x[::10], y=np.sin(x[::10]), title='plotly figure')\n",
         [display({"application/vnd.plotly.v1+json": plotly_json, "text/html": ["<div>plotly html placeholder</div>"]})], 4),
    cell("code", "import sys\nprint('to stderr', file=sys.stderr)\nfor i in range(3):\n    print('line', i)\n",
         [stream("stderr", "to stderr\n"), stream("stdout", "line 0\nline 1\nline 2\n")], 5),
    cell("code", "1 / 0\n", [{"output_type": "error", "ename": "ZeroDivisionError", "evalue": "division by zero",
                               "traceback": ["\x1b[0;31m---------------------------------------------------------------------------\x1b[0m",
                                             "\x1b[0;31mZeroDivisionError\x1b[0m                         Traceback (most recent call last)",
                                             "Cell \x1b[0;32mIn[6], line 1\x1b[0m\n\x1b[0;32m----> 1\x1b[0m \x1b[38;5;241m1\x1b[39m \x1b[38;5;241m/\x1b[39m \x1b[38;5;241m0\x1b[39m",
                                             "\x1b[0;31mZeroDivisionError\x1b[0m: division by zero"]}], 6),
    cell("markdown", "## A long cell follows\n"),
    cell("code", "".join(f"print({i})  # a fairly long line of code to exercise wrapping in narrow terminals {'x' * 40}\n" for i in range(30)), [stream("stdout", "".join(f"{i}\n" for i in range(30)))], 7),
    cell("code", "", []),
]
nb = {
    "cells": cells,
    "metadata": {
        "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
        "language_info": {"name": "python", "version": "3.12"},
    },
    "nbformat": 4,
    "nbformat_minor": 5,
}
out = sys.argv[1] if len(sys.argv) > 1 else "assets/sample.ipynb"
with open(out, "w") as f:
    json.dump(nb, f, indent=1, sort_keys=True, ensure_ascii=False)
    f.write("\n")
print("wrote", out)
