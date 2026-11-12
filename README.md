# saturn

<div align="center"><pre>
        ·                            ·        
     ⢀⡠⠤⠒⠒⣊⡩⠭⠭⢭⣭⣭⣭⣭⣭⠷⣶⠶⠦⠤⢤⣀⣀                  
✦  ⢀⠔⠁⢀⠤⣊⠭⠒⠊⠉⣉⣁⣤⣤⠞⣿  ⠙⠶⣤⣤⣀⣠⡽⠻⡵⣒⠤⢄⡀            
   ⡇ ⢀⠇⡜  ⢀⡴⠛⡩⠛⣹⡇ ⠈⠛⢦⣤⣀⡀ ⠉⠁  ⣸⣶⡫⢕⡪⣑⠢⢄⡀        
   ⢱ ⠈⡆⢇  ⢸⡇⡎ ⢰⠁⠙⢦⣄⡀  ⠉⠙⠛⠛⠛⠛⠛⠉⠈⣷⣤⡈⠑⠭⡢⣈⠒⢄      
    ⠑⢄⠈⢎⠢⡀ ⠻⣌⢆⢸⢧⡀ ⠈⠙⠛⠶⢦⣤⣤⣄⣀⣠⣤⣤⠶⡇⠱⡙⣦ ⠈⠢⡱⡀⠑⢄    
      ⠑⠤⡉⠪⣒⢄⡈⠛⢯⣷⣿⣷⣦⣤⣀⡀   ⠈⠉⠁  ⣠⠇ ⡸⢸⡇  ⢱⠸⡀ ⢇   
        ⠈⠑⠢⢍⡪⢕⣢⠬⣙⠛⠿⢿⣿⣿⣿⣿⣷⣶⣶⣶⣿⣿⣏⣤⣊⣤⠞⠁  ⡜⢰⠁ ⢸   
            ⠈⠑⠒⠭⢞⣻⠿⢶⣦⣬⣭⣉⣛⣛⣛⠛⠛⠛⠛⢉⣉⣀⡠⠤⣒⡩⠒⠁⢀⠔⠁ ✦ 
  ·               ⠉⠉⠒⠚⠫⠭⠿⢟⣛⣛⣛⣛⣛⣓⣒⣒⣊⡩⠤⠤⠒⠊⠁     
</pre></div>

A fast, keyboard-first Jupyter notebook for the terminal.

- Edits and runs `.ipynb` files against any Jupyter kernel, over ZeroMQ (no Jupyter server).
- Inline matplotlib and plotly figures (Kitty graphics, iTerm2, Sixel, or halfblocks in tmux).
  Plotly figures are interactive in the `z` popout.
- Vim editing inside cells, Jupyter-style commands around them.
- Command palette, fuzzy file picker with preview, kernel picker, themes.

## Install

```bash
make install    # builds and installs to ~/.local/bin (PREFIX=... to change)
```

Needs a Rust toolchain. Avoid `cargo install`: its copy in `~/.cargo/bin` can hide this one.
Plotly figures without a PNG are rendered by a sidecar that needs `uv` and Chrome.

## Usage

```bash
saturn notebook.ipynb           # open a notebook
saturn [dir/]                   # pick a notebook in the files picker
saturn nb.ipynb --kernel python3
```

Logs: `~/Library/Caches/saturn/saturn.log` (or `$XDG_CACHE_HOME/saturn`); `SATURN_LOG=debug` for more.
Extra kernel dirs: `SATURN_KERNEL_DIRS=/path/to/share/jupyter:...`.

## Keys

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

## Configuration

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

## Themes

`saturn theme list` shows what's available; `Ctrl+P` → "Select theme…" previews them live.
Built in: `tokyo-night`, `tokyo-night-storm`, `catppuccin-mocha`, `dracula`, `gruvbox-dark`,
`monokai`, `nord`, `one-dark`, `github-dark`, `solarized-dark`, `solarized-light`, `terminal`.

```bash
saturn theme import-nvim        # copy your Neovim colorscheme
```

Custom themes live in `~/.config/saturn/themes/<name>.toml` and can `extends = "tokyo-night"`;
see `assets/themes/` for the keys, and `saturn theme show NAME` for resolved colours.

## Development

```bash
cargo test
cargo run -- assets/sample.ipynb
```
