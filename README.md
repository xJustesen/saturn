<div align="center">
  <img alt="saturn" src="assets/logo.svg" width="520">
</div>

<h1 align="center">saturn</h1>

<p align="center">A fast, keyboard-first Jupyter notebook for the terminal.</p>

<h4 align="center">
  <a href="#-getting-started">Install</a>
  ·
  <a href="#%EF%B8%8F-keys">Keys</a>
  ·
  <a href="#%EF%B8%8F-configuration">Configure</a>
  ·
  <a href="#-themes">Themes</a>
</h4>

<div align="center"><p>
  <img alt="Built with Rust" src="https://img.shields.io/badge/built_with-Rust-ee999f?style=for-the-badge&labelColor=302D41&logo=rust&logoColor=D9E0EE" />
  <img alt="Jupyter" src="https://img.shields.io/badge/kernels-Jupyter-8bd5ca?style=for-the-badge&labelColor=302D41&logo=jupyter&logoColor=D9E0EE" />
  <img alt="License" src="https://img.shields.io/badge/license-MIT-c69ff5?style=for-the-badge&labelColor=302D41" />
</p></div>

saturn edits and runs `.ipynb` files straight from your terminal. It talks to Jupyter kernels
directly over ZeroMQ, so there is no Jupyter server to start and no browser to open: Vim
editing inside cells, Jupyter-style commands around them, and your figures inline.

## ✨ Features

- 🪐 Edit and run `.ipynb` files against any Jupyter kernel, with no Jupyter server
- 🚀 Blazingly fast: one native Rust binary
- ⌨️ Vim editing inside cells, Jupyter-style commands around them
- 📊 Inline matplotlib and plotly figures (Kitty graphics, iTerm2, Sixel, or halfblocks in tmux)
- 🖱️ Interactive plotly figures in the `z` output popout
- 🔍 Command palette, fuzzy file picker with preview, kernel picker
- 🎨 Twelve built-in themes, or import your Neovim colorscheme

## ⚡️ Requirements

- A **Rust** toolchain to build
- A **Jupyter kernel**, such as `ipykernel`
- A terminal with **Kitty graphics**, **iTerm2** or **Sixel** support for full-resolution figures
  **_(optional)_**: halfblocks are the fallback
- [`uv`](https://docs.astral.sh/uv/) and **Chrome** to render plotly figures that carry no PNG
  **_(optional)_**
- A [Nerd Font](https://www.nerdfonts.com/) for icons **_(optional)_**

## 🚀 Getting Started

```bash
make install    # builds and installs to ~/.local/bin (PREFIX=... to change)
```

Avoid `cargo install`: its copy in `~/.cargo/bin` can hide this one.

```bash
saturn notebook.ipynb               # open a notebook
saturn [dir/]                       # pick a notebook in the files picker
saturn nb.ipynb --kernel python3    # choose the kernel
```

<details><summary>Logs and kernel discovery</summary>

- Logs: `~/Library/Caches/saturn/saturn.log` (or `$XDG_CACHE_HOME/saturn`). Set
  `SATURN_LOG=debug` for more.
- Extra kernel dirs: `SATURN_KERNEL_DIRS=/path/to/share/jupyter:...`

</details>

## ⌨️ Keys

Press `?` in saturn for the full list. The essentials:

| Key | Action | Key | Action |
|---|---|---|---|
| `j` / `k` | next / previous cell | `Enter` / `i` | edit cell |
| `a` / `b` | insert cell above / below | `Esc` | leave the cell |
| `dd` / `x` / `c` / `v` | delete / cut / copy / paste | `Shift+Enter` | run and advance |
| `m` / `y` | to markdown / code | `Ctrl+C` / `00` | interrupt / restart kernel |
| `gt` | go to cell (picker) | `K` | select kernel |
| `/` | search | `Ctrl+O` | open file |
| `z` | output popout | `Ctrl+P` | command palette |
| `u` / `Ctrl+R` | undo / redo | `Ctrl+S` / `q` | save / quit |

## ⚙️ Configuration

`~/.config/saturn/config.toml` (also on macOS). Every key is optional:

```toml
theme = "tokyo-night"
line_numbers = false
relative_numbers = true
tab_width = 4
auto_pairs = true
wrap = true
max_image_rows = 20
stream_max_lines = 500
icons = true          # needs a Nerd Font
```

An invalid value makes saturn ignore the file; the log says why.

<pre>
~/.config/saturn
├── config.toml
└── themes
    └── my-theme.toml
</pre>

## 🎨 Themes

`saturn theme list` shows what's available; `Ctrl+P` → "Select theme…" previews them live.

<details><summary>Built-in themes</summary>

`tokyo-night` · `tokyo-night-storm` · `catppuccin-mocha` · `dracula` · `gruvbox-dark` ·
`monokai` · `nord` · `one-dark` · `github-dark` · `solarized-dark` · `solarized-light` ·
`terminal`

</details>

```bash
saturn theme import-nvim        # copy your Neovim colorscheme
```

Custom themes live in `~/.config/saturn/themes/<name>.toml` and can `extends = "tokyo-night"`.
See [`assets/themes/`](assets/themes) for the keys, and `saturn theme show NAME` for resolved
colours.

## 🛠️ Development

```bash
cargo test
cargo run -- assets/sample.ipynb
```
