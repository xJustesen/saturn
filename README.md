# saturn

A blazingly fast, keyboard-first Jupyter notebook for the terminal.

- Opens, edits, runs and saves `.ipynb` files (nbformat 4.5, byte-stable round trips).
- Talks to any Jupyter kernel directly over ZeroMQ (no Jupyter server needed).
- Inline figures: matplotlib PNGs and plotly figures, drawn with the Kitty graphics
  protocol (Ghostty, Kitty, WezTerm), iTerm2 or Sixel, falling back to unicode halfblocks
  (e.g. inside tmux).
- Vim-modal editing inside cells, Jupyter-style cell commands around them.
- Fuzzy command palette, file browser and kernel picker.

## Install

```bash
cargo install --path .
```

Requires a Rust toolchain. Kernels are discovered from the usual Jupyter data
directories, from `uv tool` installs (e.g. `uv tool install jupyterlab`) and from
`$VIRTUAL_ENV`; add more with `SATURN_KERNEL_DIRS=/path/to/share/jupyter:...`.

Plotly figures that arrive without a PNG are rendered by a sidecar started
with `uv run --no-project --with plotly --with kaleido` (needs `uv` and Chrome/Chromium).
It keeps one headless Chrome for the whole session, so after the first figure
(which pays for the browser launch) renders take tens of milliseconds. The sidecar
is started early when the notebook already holds plotly outputs or a code cell
mentions plotly, and otherwise on first use.
It runs in its own throwaway environment, so it never touches the notebook project's
`uv.lock` or `.venv`, and the project's plotly version doesn't matter.
If the kernel environment has `kaleido` installed, plotly emits PNGs itself and the
sidecar is never started.

## Usage

```bash
saturn notebook.ipynb        # open a notebook (kernel from its metadata)
saturn                       # browse the current directory
saturn dir/ --kernel python3 # browse a directory, force a kernelspec name
```

Logs go to `~/Library/Caches/saturn/saturn.log` (or `$XDG_CACHE_HOME/saturn`);
`SATURN_LOG=debug` raises the level. Kernel stdout/stderr land in
`kernel-<name>.log` next to it.

## Keys

Notebook (command mode):

| Key | Action | Key | Action |
|---|---|---|---|
| `j` / `k`, `↓` / `↑` | focus next / previous cell | `Enter` / `i` | edit cell (vim normal / insert) |
| `gg` / `G` | first / last cell | `Esc` | (in vim normal mode) back to notebook |
| `Ctrl+D` / `Ctrl+U` | half page down / up | `Shift+Enter` | run and advance |
| `a` / `b` | insert cell above / below | `Ctrl+Enter` | run |
| `dd` / `x` / `c` | delete / cut / copy cell | `Alt+Enter` | run and insert below |
| `v` / `V` | paste below / above | `Ctrl+C` / `I` | interrupt kernel |
| `Ctrl+K` / `Ctrl+J` | move cell up / down | `00` | restart kernel |
| `m` / `y` / `r` | to markdown / code / raw | `K` | select kernel |
| `M` | merge with cell below | `Ctrl+S` | save |
| `Ctrl+_` | split cell at cursor (while editing) | `Ctrl+O` | open file / browse |
| `u` / `Ctrl+R` | undo / redo cell operation | `Ctrl+P` | command palette |
| `o` / `O` | toggle (or expand) / clear outputs | `?` | help |
| `z` | inspect output in a popout | `q` | quit |

Inside a cell the editor is vim-modal (`hjkl`, `dd`, `yy`, `p`, `ciw`, `v`, `/`, `u`, …).
`Shift+Enter`, `Ctrl+Enter`, `Alt+Enter`, `Ctrl+S`, `Ctrl+P`, `Ctrl+C` and `Ctrl+Q`
work in every mode. Shift+Enter needs a terminal with the kitty keyboard protocol
(Ghostty, Kitty, WezTerm, foot); elsewhere use `Alt+Enter` or `Ctrl+Enter`.

With a plotly figure that could not be rendered, `o` writes its HTML to the cache
directory and opens it in the browser.

`z` opens the focused cell's output in a popout that fills the terminal and follows
it when it is resized. Figures are drawn as large as the popout allows: plotly figures
are re-rendered by the sidecar at the popout's pixel size, PNGs are scaled up (at most
4×). Text outputs are shown in full, without truncation. Inside the popout: `j`/`k`,
`Ctrl+D`/`Ctrl+U`, `g`/`G` scroll, `w` toggles wrapping, `h`/`l` pan when unwrapped,
`Tab`/`Shift+Tab` (or `n`/`p`) switch between the cell's outputs, `Esc`/`q`/`z` close.

## Configuration

`~/.config/saturn/config.toml` (all keys optional):

```toml
max_image_rows = 20            # tallest inline image, in rows
max_image_rows_halfblocks = 12 # same when only halfblocks are available
stream_max_lines = 500         # longer outputs are truncated until expanded with `o`
persist_plotly_png = true      # store sidecar PNGs in the notebook so they survive
plotly_scale = 2.0             # pixel scale for plotly renders
theme = "tokyo-night"          # see Themes below
line_numbers = false
tab_width = 4
wrap = true
show_hidden = false            # file browser
```

On macOS the same `~/.config/saturn/` directory is used (`$XDG_CONFIG_HOME` is honoured).

## Themes

`theme` names a built-in theme, a file in `~/.config/saturn/themes/<name>.toml`, a
path to a `.toml` file, or (syntax colors only) one of syntect's bundled themes.
`saturn theme list` prints everything the config accepts. In saturn, `Ctrl+P` →
“Select theme…” opens a picker with live preview: the theme follows the cursor,
`Enter` keeps it for the session (the status bar shows the config line to make it
permanent), `Esc` reverts to the previous one.

Built in: `tokyo-night`, `tokyo-night-storm`, `monokai`, `catppuccin-mocha`, `dracula`,
`gruvbox-dark`, `nord`, `one-dark`, `solarized-dark`, `solarized-light`, `github-dark`
and `terminal` (your terminal's 16 colors, no background painted).

### Adopting your Neovim colorscheme

```bash
saturn theme import-nvim                      # the colorscheme your init.lua sets
saturn theme import-nvim --colorscheme nord   # any installed one; --name picks the file name
```

This runs `nvim --headless` with your own config, reads the resolved highlight
groups (treesitter captures such as `@keyword.import` first, legacy groups like
`Keyword` as fallback) and writes `~/.config/saturn/themes/<colorscheme>.toml`.
Put `theme = "<colorscheme>"` in the config to use it. The file is a normal theme
file, so tweak it freely; re-run with `--force` to regenerate.

### Writing a theme

A theme file has three optional tables. `extends = "name"` inherits every key of
another theme first (built-in or user), so a tweak can be a few lines:

```toml
name = "My Tokyo"
extends = "tokyo-night"

[ui]
background = "none"            # keep the terminal's background
focus = "#ff9e64"

[syntax]
comment = { fg = "#565f89", italic = true }
"keyword.import" = "#7dcfff"   # keys with dots must be quoted

[scopes]                       # raw syntect scope selectors, for anything else
"meta.function-call.arguments" = "#c0caf5"
```

Colors are `#rrggbb`, `#rgb`, an ANSI name (`red`, `light blue`, `dark gray`),
a 0–255 index, or `none` (terminal default). A style is either a color or a table
with `fg`, `bg`, `bold`, `italic`, `underline`.

`[ui]` keys: `background`, `foreground`, `muted` (hints, raw cells), `line_numbers`,
`selection`, `cursor_line`, `input_bg` (full-row tint behind code inputs, so they
stand out from outputs), `focus` (bar on the focused cell, borders), `edit` (bar
while editing), `error`, `warning`, `info`, `success`, `status_bg`, `status_fg`,
`border`, `float_bg`, `match_bg`, `match_fg` (picker selection), `mode_fg`,
`mode_command`, `mode_normal`, `mode_insert`, `mode_visual`, `mode_search`. Unset
keys fall back to sensible relatives (`border` → `focus`, `mode_insert` → `edit`,
`input_bg` → a shade of `background`, …). With no `background` (and no `input_bg`)
there is no tint, and unfocused cells show a dim bar instead.

`[syntax]` keys (named like treesitter captures): `comment`, `comment.doc`, `keyword`,
`keyword.control`, `keyword.import`, `keyword.return`, `keyword.exception`,
`keyword.function`, `keyword.operator`, `operator`, `string`, `string.escape`,
`string.regexp`, `number`, `boolean`, `constant`, `constant.builtin`, `variable`,
`variable.builtin`, `parameter`, `property`, `function`, `function.call`,
`function.builtin`, `method`, `constructor`, `type`, `type.builtin`, `module`,
`decorator`, `punctuation`, `punctuation.bracket`, `punctuation.special`, `tag`,
`attribute`, `markup.heading`, `markup.strong`, `markup.emphasis`, `markup.raw`,
`markup.link`, `markup.list`, `markup.quote`, `invalid`. More specific keys win
over broader ones (`keyword.import` over `keyword`); unset keys inherit the
foreground. `saturn theme show NAME` prints the resolved colors and the syntect
selectors each key maps to.

While a cell is being edited, the editor shows the theme's colors but not its
bold/italic attributes (a limitation of the editor widget); unfocused cells show both.
Rendered markdown cells keep their own fixed styling.

## Development

```bash
cargo build
cargo test
cargo run -- assets/sample.ipynb
```

`assets/make_sample.py` regenerates the sample notebook (needs matplotlib, plotly, pandas).
