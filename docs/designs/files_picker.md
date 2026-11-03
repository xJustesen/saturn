# Files picker with preview

*   **Author:** xJustesen
*   **Status:** Proposed (revision 3, after the Stage 9 re-audit)
*   **Date:** 2026-11-03

--------------------------------------------------------------------------------

## Overview

saturn is a terminal Jupyter notebook. It is a Rust program built on ratatui.
It opens, edits, runs and saves `.ipynb` files, and it talks to Jupyter kernels
directly.

Today, `Ctrl+O` opens a file browser that shows one directory at a time and
lists only subdirectories and `.ipynb` files. This design replaces it with a
files picker modelled on the "files" picker in
[snacks.nvim](https://github.com/folke/snacks.nvim), the picker that
[LazyVim](https://www.lazyvim.org/) uses for `<leader>ff`.

*   **The popup.** The picker is a popup with two panes.
*   **Left pane.** Every file under the git repository root, in one flat list.
    Typing a fuzzy query narrows the list.
*   **Right pane.** A preview of the selected file. Text files are shown
    highlighted, with line numbers. Notebooks are shown as their code and
    markdown cells, without outputs.
*   **Opening.** `Enter` on a notebook opens it.
*   **Startup.** The same picker is the first screen when saturn starts without
    a notebook (`saturn`, or `saturn dir/`).

## The Problem

The current browser (`open_browser`, `src/app.rs:1509`) has two gaps:

1.  **No preview.** To learn what a notebook contains, the user has to open it.
    Opening a notebook replaces the current one. If the current one has unsaved
    changes, saturn asks whether to discard them first. So checking "is this the
    right notebook?" costs a prompt and a round trip.
2.  **No overview.** The browser lists one folder at a time.
    *   In a repository with notebooks spread across `research/`, `scratch/`
        and `notebooks/<topic>/`, the user walks folder by folder (`Enter`,
        `Backspace`, `Enter`) to find a file.
    *   The user cannot see what sits next to a file in other folders.
    *   Non-notebook files (`.py`, `requirements.txt`, data files) are not
        listed at all, so the browser does not show the project as it is.

The solution has three parts:

*   list every file in the repository at once;
*   narrow the list by typing part of a path;
*   show the selected file's contents next to the list before anything is
    opened.

## Goals

*   **G1 — One list for the whole repository.** `Ctrl+O` lists every file under
    the root as one flat list.
    *   The root is the git repository root that contains the open notebook's
        folder, or the current directory when no notebook is open.
    *   Outside any git repository, the root is that starting folder.
    *   The root stays fixed while the picker is open.
*   **G2 — Find a file by typing part of its path.**
    *   Typing narrows the list by fuzzy matching on the path relative to the
        root.
    *   A `matched/total` counter shows how many files match.
*   **G3 — See a file before opening it.** The selected file is previewed as
    follows:
    *   **Text files:** syntax-highlighted, with line numbers.
    *   **Notebooks:** code and markdown cells in order, with all outputs
        removed.
    *   **Everything else:** a one-line placeholder that says why there is no
        preview. This covers binary files, special files (pipes, sockets,
        devices), symlinks to folders and files over the size limit.
*   **G4 — Keep noise out by default, and let the user bring it back.**
    *   Files matched by `.gitignore`, and hidden (dot-named) files and folders,
        are left out by default.
    *   One key shows hidden files and another shows ignored files.
    *   `.git` is never listed.
    *   `.ipynb` files are always listed, even inside gitignored folders,
        because notebooks in folders like `scratch/` are still the user's own
        work.
*   **G5 — No file-system call on the UI thread.** While the picker is open,
    root discovery, scanning, `stat` calls and preview reads all run on worker
    threads.
    *   Keys are handled while a scan or a preview build is in progress,
        including at startup in `~`.
    *   A worker blocked on a hung read never stops saturn from exiting.
*   **G6 — Open notebooks from it.**
    *   `Enter` on a `.ipynb` opens it, through the existing unsaved-changes
        confirm.
    *   `Enter` on any other file keeps the picker open and shows, inside the
        picker, that saturn opens only `.ipynb` files.
*   **G7 — Replace, do not add.**
    *   The one-folder browser and its code are removed in the same change.
    *   `Ctrl+O`, `saturn` and `saturn dir/` all open the new picker.

**How success is checked:**

*   **Scan time.** On a repository with about 1,200 files after `.gitignore`,
    the scan finishes within 100 ms of `Ctrl+O`. This is measured by the
    scan's `info!` log line.
*   **No slow frames.** No frame drawn while the picker is open exceeds 8 ms.
    The existing "slow frame" debug log line must not appear.
*   **Preview time.** At the same scale, each preview build takes under 50 ms.
    This is measured by the preview `debug!` log line, with `SATURN_LOG=debug`.
*   **Startup in `~`.** Starting `saturn` in `~` draws the picker on the first
    frame, and `Esc` closes it while the scan is still running.
*   **Finding notebooks.** For every notebook in the test repository, typing its
    file name without the extension puts it among the top five matches.

## Non-Goals

*   **Opening or editing non-notebook files.** There is no `$EDITOR` hand-off.
    `Enter` on a `.py` file only shows the notice.
*   **Outputs or figures in the notebook preview.** No images, tables or
    tracebacks are shown. This keeps the preview text-only and avoids drawing
    terminal images in a pane that changes on every key press.
*   **Image previews.** PNG, JPEG and similar files get the binary placeholder.
*   **Previewing v3 notebooks.** saturn can open them, but the preview shows a
    placeholder.
*   **Changing the root from inside the picker.** To list a different folder,
    start saturn there or open a notebook there.
*   **Creating notebooks from the picker.** The palette command creates the
    notebook in the open notebook's folder, or in the current directory when no
    notebook is open. Unlike the old `[ new notebook here ]` row, it does not
    use the folder being browsed. The existing "New notebook" palette
    command (`Ctrl+P`) does this.
*   **Entering symlinked folders.** They are listed as entries, but the scanner
    does not follow them into their targets.
*   **File operations:** create, rename, delete, move or copy.
*   **Searching file contents** (a grep picker), multi-select and recent files.
*   **A tree view** (neo-tree style) with folders that expand.
*   **Scanning beyond the caps.** The scan stops at fixed caps. Roots larger
    than that, such as `/`, are not a target.
*   **Watching the file system.** Files created after a scan appear only when
    the picker is reopened or the hidden toggle is changed. Changing the
    hidden toggle starts a new scan.

## Open Questions

None. These were settled during drafting and review:

*   **The root:** the git repository root, fixed while the picker is open.
*   **The file list:** all files are listed, and gitignored notebooks are
    always listed.
*   **Ignore rules:** saturn applies `.gitignore` rules itself, in one walk. The
    ignored toggle is then an instant refilter. See Alternative F for what this
    costs.
*   **Hidden folders:** they are pruned while the hidden toggle is off, and the
    toggle starts a new scan.
*   **Previews:** they are built on a worker thread, which keeps its own cache.
*   **Icons:** Nerd Font icons, behind an on/off setting.
*   **Syntax highlighting:** the two-face syntax set is used app-wide.
*   **After the discard confirm:** answering `n` or `Esc` reopens a fresh
    picker.

## Critical User Journeys (CUJs)

The file names below are examples.

### CUJ 1: Find and open a notebook in another folder

*   **Given**: `research/summary.ipynb` is open and saved. The
    repository has notebooks in several folders.
*   **When**: The user presses `Ctrl+O`, types `wea exp`, moves to
    `notebooks/weather/explore.ipynb` and presses `Enter`.
*   **Then**:
    *   The list showed every file in the repository, narrowed to paths that match
        `wea exp`.
    *   The right pane showed the cells of `explore.ipynb`, without outputs.
    *   `explore.ipynb` is now open, and the picker is closed.

### CUJ 2: Browse previews while the current notebook has unsaved changes

*   **Given**: The open notebook has unsaved edits.
*   **When**: The user opens the picker, moves through five notebooks while
    reading their previews, then presses `Esc`.
*   **Then**: Nothing was opened and nothing was asked. The open notebook and
    its edits are unchanged.

### CUJ 3: Decline to discard changes

*   **Given**: The open notebook has unsaved edits. The picker is open, and
    `explore.ipynb` is selected.
*   **When**: The user presses `Enter`, and then `n` at "Discard unsaved
    changes?".
*   **Then**:
    *   A fresh picker opens at the same root, with `explore.ipynb` selected.
    *   The query is empty.
    *   The open notebook and its edits are unchanged.

### CUJ 4: Look at a non-notebook file

*   **Given**: The picker is open at the repository root.
*   **When**: The user selects `requirements.txt`, then presses `Enter`.
*   **Then**:
    *   The preview showed the file with line numbers.
    *   After `Enter`, the picker is still open with the same query and
        selection.
    *   Its footer shows "saturn opens only .ipynb files".

### CUJ 5: Find a notebook inside a gitignored folder

*   **Given**: The repository's `.gitignore` lists `scratch/`.
    `scratch/try.ipynb` exists, next to 300 `.csv` files.
*   **When**: The user opens the picker and types `try`.
*   **Then**:
    *   `scratch/try.ipynb` is listed and previewed.
    *   The `.csv` files in `scratch/` are not listed until the user presses
        the "show ignored" key.
    *   That key shows them at once, without a new scan.

### CUJ 6: Start saturn in a large folder

*   **Given**: The user runs `saturn` in `~`, which holds hundreds of thousands
    of files.
*   **When**: saturn starts.
*   **Then**:
    *   The picker is drawn on the first frame, with a `scanning…` marker.
    *   Files appear in batches, and the counter grows until the scan ends or
        a cap is hit. A capped total shows a `+` (`412/50000+`).
    *   `Esc`, typing and moving all work during the scan.
    *   Dot-folders and the folders on the skip list (for example `~/Library`)
        are not entered.
    *   macOS may ask once for access to protected folders (Desktop, Documents,
        Downloads). The prompt blocks only the scan. A denial counts as
        "unreadable".

### CUJ 7: Narrow terminal

*   **Given**: The terminal is 80 columns wide and 40 rows tall.
*   **When**: The user opens the picker.
*   **Then**:
    *   The list is on top and the preview is below it.
    *   At 100 columns by 20 rows, only the list is shown, and no preview is
        built.
    *   At 120 columns or more, the panes are side by side.

## Technical Plan (Architecture)

### Components and data flow

The picker is a new overlay, `Overlay::Files(Box<FilePickerState>)`. It sits
next to two existing overlays:

*   the `gt` cell picker (`Overlay::Cells`), which lists every cell on one row
    so the user can jump to one;
*   the `z` popout (`Overlay::Popout`), which shows one output full screen.

Two worker threads feed the picker:

*   a **scanner**, which finds the root and lists the files;
*   a **preview worker**, which builds previews and keeps a cache of them.

Both are plain `std::thread`s, not tokio tasks. When `main` drops the tokio
runtime (`src/main.rs:152-161`), the drop waits for any `spawn_blocking` task
still running. A scanner stuck in `read_dir` on a hung mount would then keep
saturn from exiting. Detached threads do not have this problem.

Both workers report through a sink closure. In the app, the closure wraps each
event in `AppEvent::Files(FilesEvent)` and sends it on the app's existing event
channel. In tests, it collects the events into a list. Neither worker touches
app state.

```text
                     Ctrl+O / saturn / saturn dir/
                                 │
                                 ▼
                   App::open_file_picker(start, select)
                                 │
  ┌──────────────────── FilePickerState (UI thread) ──────────────────────┐
  │ root (from Root event), scan_id, entries, filtered (stale), selected  │
  │ user_moved, query, prefs, current preview, notice                     │
  └──────┬──────────────────────────────▲───────────────┬────────▲───────┘
         │ ScanRequest {scan_id, start, │ FilesEvent:   │ Preview │ FilesEvent:
         │  open_path, prefs, cancel}   │ Root / Batch /│ Request │ Preview {id}
         ▼                              │ Done          ▼         │
  ┌─ scanner (std::thread) ─┐           │  ┌─ preview worker (std::thread) ─┐
  │ find_root, canonicalize │───────────┘  │ latest-request slot            │
  │ ignore::WalkBuilder     │   sink →     │ stat → LRU(32) → build on miss │
  │ + saturn's ignore stack │   AppEvent   │ classify → text / notebook     │
  └─────────────────────────┘   channel    └────────────────────────────────┘
```

New code lives in these places:

*   **`src/files/`**: listing and previews. It has no drawing code, although
    `preview.rs` and `outline.rs` produce ratatui `Line`s, because previews are
    styled text.
    *   `root.rs`: root discovery.
    *   `skip.rs`: the built-in skip list.
    *   `walk.rs`: the scanner and its ignore rules.
    *   `preview.rs`: classification, building and the preview worker.
    *   `outline.rs`: the notebook reader.
*   **`src/ui/file_picker.rs`**: the picker's state, key handling and drawing,
    in one file like `cell_picker.rs`.
*   **`src/ui/icons.rs`**: the file-type icon table.
*   **`src/ui/fuzzy.rs`**: the fuzzy-matching helper, shared by the palette, the
    cell picker and the files picker.
*   **`src/syntax.rs`**: the app's single syntax set.

### Finding the root

The scanner thread does root discovery as its first step, so the UI thread does
no file-system work. Until the root is known, the picker's title shows the
starting folder.

`find_root(start)` works like this:

1.  It canonicalizes `start`.
2.  It walks up from there to the first directory that contains an entry named
    `.git`. That entry may be a directory. In git worktrees (this repository is
    one) and submodules, it is a *file*.
3.  That directory is the **work tree**, and the root is the work tree.
4.  If no `.git` is found, the root is the canonicalized `start`, and there is
    no repository.

`start` depends on how the picker was opened:

| Entry point | `start` |
|---|---|
| `Ctrl+O` with a notebook open | the notebook's folder |
| `Ctrl+O` with no notebook path | the current directory |
| `saturn` | the current directory |
| `saturn dir/` | `dir/` |
| reopened after declining the discard confirm | the same `start` as before |

So `saturn ~` lists `~` unless `~` is itself inside a repository. The skip list
and the caps bound that scan.

The root itself is never pruned, and it is never treated as hidden or ignored.
So `saturn ~/.dotfiles` lists the files inside `~/.dotfiles`.

The scanner also canonicalizes the open notebook's path. It sends the
canonical root and the canonical path to the picker together, in one
`FilesEvent::Root`. The picker needs that path to preselect the open notebook
and to mark its preview as the on-disk version.

### Scanner

The scanner walks with the `ignore` crate's `WalkBuilder`, with every built-in
filter turned off:

*   `standard_filters(false)`;
*   `follow_links(false)`;
*   `same_file_system(true)`;
*   `sort_by_file_name`.

So the walk is depth-first, and the entries of each folder are sorted by file
name. saturn applies the ignore rules itself, so it can tag an ignored file
instead of dropping it. That is what lets gitignored notebooks be listed.

**Pruning.** `filter_entry` stops the walker from entering a path, or from
listing it, in these cases:

*   **Any entry named `.git`**, file or folder.
*   **Hidden folders and files**: names that start with `.`, while the hidden
    toggle is off. The root itself is never pruned.
*   **Skip-list folders.** These are pruned even with both toggles on:
    *   the names `node_modules`, `.venv`, `venv`, `__pycache__`,
        `.ipynb_checkpoints`, `site-packages` and `.tox`;
    *   any folder that contains a `CACHEDIR.TAG` file, which Cargo's `target/`
        and the pytest, ruff and mypy caches all write;
    *   any folder that contains `pyvenv.cfg`, which marks a Python virtual
        environment of any name;
    *   `~/Library` and `~/.Trash`.

The skip list uses the marker files instead of a bare name like `target`. That
way, a user's own folder that happens to be called `target` is still listed.

Hidden folders are pruned, not tagged, because no rule needs anything from
inside them. In `~`, the dot-folders (`.cargo`, `.rustup`, `.npm`, `.cache`)
would otherwise use up most of the visit cap before the first file to show.
The cost is that the hidden toggle starts a new scan instead of refiltering.

**Ignore rules.** saturn applies git's ignore rules itself, with the
`ignore::gitignore` matcher for each file. The rules come from three sources,
in git's order of precedence:

1.  **`.gitignore` files.** Each folder visited may have one. A deeper file
    wins over a shallower one.
2.  **`info/exclude`.** It is loaded only inside a repository. For a worktree,
    the scanner reads the `gitdir:` line in the `.git` file, and then follows
    the `commondir` file in that folder to the shared git folder.
3.  **The global excludes file** (`core.excludesFile`). It is loaded only
    inside a repository, and it is built relative to the work tree.

An entry is **ignored** when either of these is true:

*   **Its parent folder is ignored.** git never re-includes a file inside an
    excluded folder, so this is inherited.
*   **A rule says to ignore it.** The scanner checks the `.gitignore` files
    from the deepest matching one upwards, then `info/exclude`, then the global
    file. The first source with a matching rule decides. Inside one file, the
    last matching rule wins, as `Gitignore::matched` already does. A `!` rule
    means "not ignored".

Two edge cases:

*   **Outside a repository:** `.gitignore` files still apply, but there is no
    `info/exclude` and no global file. This makes an unpacked project without
    `.git` behave like its repository.
*   **A repository nested inside the root:** treated as a plain folder. Its own
    `.gitignore` files apply, and its `.git` is pruned, but its `info/exclude`
    is not read.

**What the scanner records.** Each file becomes one entry. Folders are not
entries.

```text
FileEntry {
    path:     PathBuf,     // original bytes, used to open the file
    display:  Box<str>,    // path relative to root, lossy UTF-8, '/'-separated,
                           // control characters replaced one-for-one
    hay:      Utf32String, // fuzzy haystack built from `display`
    name_at:  u32,         // byte offset in `display` where the file name starts
    ignored:  bool,
    notebook: bool,        // extension == "ipynb"
}
```

**Terms.** These words have fixed meanings in this design:

*   An entry is **listed by default** when `!ignored || notebook`.
*   With the ignored toggle on, every entry is **visible**.
*   With it off, only the entries listed by default are visible.
*   Hidden entries never reach the list unless the hidden toggle was on when
    the scan ran.

**Caps.** Three caps bound memory and time:

*   **`SHOWN_CAP = 50_000`:** the most entries listed by default. When it is
    reached, the walk stops.
*   **`EXTRA_CAP = 50_000`:** the most ignored non-notebook entries kept. When
    it is reached, the walk goes on, but later ignored non-notebook files are
    only counted, not kept. This stops a gitignored `data/` folder with 80,000
    files from taking over memory.
*   **`VISIT_CAP = 1_000_000`:** the most walker entries visited, folders
    included. When it is reached, the walk stops. This bounds a scan of `~`.

When any cap is hit, the counter's total is shown with a trailing `+`.

**Protocol.**

```text
FilesEvent::Root  { scan_id, root: PathBuf, open_path: Option<PathBuf> }   // first event of a scan
FilesEvent::Batch { scan_id, entries: Vec<FileEntry> }   // ≤ 512 entries, or 30 ms after the last send
FilesEvent::Done  { scan_id, summary: ScanSummary }      // counts, caps hit, unreadable, elapsed
```

*   **Generation and preview ids.** `scan_id` and the preview `id` both come from
    one process-wide `AtomicU64`. A scan or preview from a picker that has
    already closed can never carry a number that the new picker is waiting
    for.
*   **Batching.** Batches exist because the event loop handles at most 256
    events per frame (`src/app.rs:227-234`). One event per file would lag.
*   **Cancelling.** Each scan has an `Arc<AtomicBool>` cancel flag. The scanner
    checks it after every walker entry; the check is a single atomic load. The
    flag is set when the picker is dropped and when a new scan replaces this
    one (the hidden toggle). After that, the scanner returns without sending
    `Done`.
*   **Stale events.** The picker drops any event whose `scan_id` is not its current
    one. An event that arrives when no picker is open is dropped by the app.
*   **Errors.** Unreadable folders (permission denied, a folder that vanished)
    are counted in `ScanSummary.unreadable`. One `warn!` per scan lists the
    first five paths. The picker never shows a message for each one.
*   **Redraws.** The 200 ms `Tick` marks the UI dirty while a scan is running,
    so the `scanning…` marker and the counter update.

### Filtering and selection

The picker keeps `filtered`, the list of visible entries that match the query,
in rank order. It never edits this list in place. Any of these marks it stale:

*   a batch arriving;
*   a query edit;
*   the ignored toggle.

`filtered` is rebuilt once, at the start of the next draw. The event loop
handles up to 256 events before each draw, so a burst of keys and batches
costs one scoring pass.

*   **The shared helper.** `ui/fuzzy.rs` wraps nucleo-matcher.
    *   The files picker uses `MatcherConfig::DEFAULT.match_paths()`.
    *   Results are ranked by score. Equal scores are broken first by a length
        the caller passes in (shorter first), then by walk order.
    *   The palette and the cell picker pass a length of 0 and keep their
        current haystacks, so their results do not change.
*   **Empty query.** All visible entries, in walk order.
*   **Selection.** `selected` holds an entry index, not a row number. A move
    sets the `user_moved` flag. A query edit or either toggle clears it. Each
    rebuild of `filtered` applies the first of these steps that holds:
    1.  `user_moved` is set, and the selected entry is still in `filtered`:
        keep it.
    2.  The query is empty, and `preselect` (the file chosen before declining
        the discard confirmation prompt) is visible: select it.
    3.  The query is empty, and the open notebook is visible: select it. This
        includes an open notebook that arrives in a later batch.
    4.  Otherwise: select the top row, or nothing when the list is empty.

    So with a query set and no move, the selection follows the top match as
    better matches arrive.
*   **Match highlights.** Positions are computed only for the rows on screen,
    and only when `display` is ASCII. nucleo turns a non-ASCII path into one
    character per grapheme, so its positions do not map onto `display`. Those
    rows are drawn without highlights.
*   **Cost.** Re-scoring 1,200 paths takes well under a millisecond. A
    50,000-entry `~` scan with a query costs tens of milliseconds per rebuild,
    and only during the scan. That is outside the target scale and accepted.

### Preview worker

There is one preview worker thread per picker. It waits on a **latest-request
slot**: a `Mutex` holding the pending request and a `closed` flag, with a
`Condvar`. `closed` is kept under the same lock as the request, so the worker
cannot miss the wake-up that tells it to stop.

*   **Requests.** Only the picker's draw step sends requests, and only while
    the preview pane is visible. A request goes out when one of these holds:
    *   the selected entry changes;
    *   the pane appears;
    *   the pane width changes while a notebook is selected;
    *   nothing is shown yet.

    A new request replaces one that is still waiting, so holding `↓` builds at
    most one preview per finished build.
*   **The cache lives in the worker.** On each request, the worker takes these
    steps:
    1.  It calls `metadata()` on the path.
    2.  It looks up `(path, mtime, len, width)` in an LRU cache of 32 entries.
        `width` is used for notebooks only, because rendered markdown wraps to
        the pane width.
    3.  It builds the preview only on a miss.

    Doing this in the worker keeps the UI thread off the disk. The picker
    always sends a request.
*   **Results.** Each result carries the request's `id`.
    *   The picker shows a result only if its `id` matches the latest request.
    *   Until that result arrives, the pane shows the previous preview, dimmed.
*   **Stopping.** The worker stops when its handle is dropped, together with
    the picker. Under the lock, `closed` is set, and the `Condvar` is notified.
    The worker's loop then returns.
*   **Hung reads.** A read or `stat` that never returns ties up only this thread.
    The preview pane stops updating. Keys, the list and quitting keep working.

```text
PreviewRequest { id: u64, path: PathBuf, width: u16, syn: Arc<SynTheme>, ui: UiColors, tab_width: usize }
FilesEvent::Preview { id: u64, preview: Preview }
Preview = Doc { lines: Vec<Line<'static>>, line_numbers: bool, note: Option<String> }
        | Placeholder(String)
UiColors { muted: Color, input_bg: Option<Color>, line_numbers: Color }
```

**Classification.** The worker runs these checks in order:

| # | Check | Result |
|---|---|---|
| 1 | `metadata()` fails | `cannot read: <error>` (a broken link, a deleted file, permission denied) |
| 2 | Is a folder (only a symlink to a folder can reach this) | `[folder]` |
| 3 | Not a regular file (pipe, socket, device) | `[fifo]`, `[socket]` or `[device]`. **The file is never opened.** |
| 4 | A NUL byte in the first 8 KiB | `binary · <size>` |
| 5 | `.ipynb` larger than `NB_MAX` (32 MB) | `notebook too large to preview (<size>)` |
| 6 | `.ipynb` | Notebook preview (below) |
| 7 | Other file larger than `TEXT_MAX` (256 KB) | `too large to preview (<size>)` |
| 8 | Anything else | Text preview (below) |

`<size>` is formatted by the existing `human_size` (`48.0M`, `2.3K`). Large
text files get no preview at all. The reason is to keep each build bounded and
predictable, and because a partial view of a large file is rarely what the user
is looking for. Showing the first part instead is listed under Future
Improvements.

**Text preview.**

1.  **Read.** At most `TEXT_MAX` bytes.
2.  **Decode** as lossy UTF-8.
3.  **Clean the text.** Remove control characters and escape sequences (see
    *Display safety* below).
4.  **Limit length.** Keep the first 1,000 lines, and cut each to 1,000
    characters. When lines were dropped, `note` reads
    `first 1000 of <n> lines`.
5.  **Pick a syntax**, in this order: by extension, by whole file name
    (`Makefile`, `Dockerfile`), by the first line (a shebang), and finally
    plain text.
6.  **Highlight** with `ui::text::highlight_with`.
7.  **Line numbers** are drawn at draw time.

**Notebook preview.**

*   **Reading.** `files/outline.rs` reads only what the preview needs: each
    cell's type and source, the notebook language, and `nbformat`. serde skips
    `outputs` without building them, so megabytes of base64 images are scanned
    past but never stored.
*   **No `nbio::load`.** It builds every output, and it adds an empty cell to a
    notebook that has none.
*   **No preview:** an `nbformat` below 4 gives
    `v3 notebook: no preview (saturn can open it)`. A parse error gives
    `cannot preview: <error>`.
*   **Each cell:**
    *   a separator line, `── [n] code ──` or `── [n] markdown ──`, in the
        muted colour. `n` is the 1-based cell number that `gt` uses.
    *   **Code cells:** cleaned, cut to 1,000 characters per line, then
        highlighted in the notebook's language on the input background colour.
    *   **Markdown cells:** cleaned, then rendered with the existing
        `render::render_markdown` at the pane width, because the preview is
        meant to look like a rendered notebook.
    *   **Raw cells:** plain text in the muted colour.
*   **Limits.** The output stops after 2,000 lines. `note` then reads
    `first 2000 lines (cells 1–<k> of <n>)`.
*   **Unsaved edits.** When the selected file is the open notebook and that
    notebook has unsaved edits, the picker adds
    `on-disk version — unsaved edits not shown` at draw time. It is added at
    draw time so the cached preview stays independent of the dirty flag.

### Display safety

Every string drawn from a file name or file contents is cleaned first, so that
a file cannot send escape codes to the terminal. There are two helpers in
`ui/text.rs`:

*   **`clean_text`, for preview content:**
    *   `\r\n` becomes `\n`. A lone `\r` is removed.
    *   Tab is kept. The highlighter expands it later.
    *   Every other C0 control (U+0000–U+001F) and C1 control
        (U+0080–U+009F) is removed. The same goes for DEL (U+007F) and for the
        bidirectional-text controls U+202A–U+202E and U+2066–U+2069.
    *   Escape sequences are removed whole:
        *   **CSI** (`ESC [`): removed up to and including the final byte,
            0x40–0x7E.
        *   **OSC, DCS, SOS, PM, APC** (`ESC ]`, `ESC P`, `ESC X`, `ESC ^`,
            `ESC _`): removed up to and including BEL or `ESC \`.
        *   **Any other `ESC`** is removed together with the one character
            after it.
        *   **An unterminated sequence** is removed up to the end of its line.
*   **`clean_name`, for list rows, the root in the title and the empty-state
    text:** it replaces each character in a fixed set with one `?`, so the
    match positions computed from `display` still line up.
    *   The set is every character that `clean_text` removes, plus tab, `\r`
        and `\n`: C0, DEL, C1 and the bidi controls.
    *   Escape sequences are not parsed. Each ESC byte becomes `?`.
    *   Replacing the bidi overrides also stops a file name from displaying
        its characters in a misleading order.

### Layout and drawing

*   **Popup size.** The popup is centred and fills 90 % of the width and 85 % of
    the height of the area above the status bar.
*   **Notebook hidden.** While the picker is open, `ui::draw` does not draw the
    notebook underneath, as it already does for the popout
    (`src/ui/mod.rs:24`). This stops sixel and iTerm2 images from showing
    through.
*   **Panes.** The layout depends on the popup's inner size:
    *   **Inner width ≥ 100 columns:** the list on the left (45 %), the preview
        on the right (55 %). That is a terminal about 114 columns or wider.
    *   **Narrower, with an inner height ≥ 20 rows:** the list on top (40 %),
        the preview below.
    *   **Otherwise:** the list only, and no preview requests are sent.
*   **Header row.** The query after `> `, with the counter on the right. A
    `scanning…` marker sits next to the counter while the scan runs.
    *   **Counter `matched/total`:** `total` is the number of visible entries
        under the current toggles. `matched` is how many of them match the
        query.
*   **List rows.** Each row has these parts:
    *   a 2-column icon slot, when icons are on;
    *   the folder part of `display`, in the muted colour;
    *   the file name, in the normal text colour;
    *   matched characters, in the focus colour.

    When a row is too long, the folder part is cut from the left with `…`, and
    the file name stays whole.
*   **Footer row.** A key hint line:
    `M-h hidden [off]  M-i ignored [off]  ^F/^B scroll  Esc close`.
    *   When the line is too wide, items are dropped from the right.
    *   The same row shows a notice for 3 seconds, such as "saturn opens only
        .ipynb files". The footer is inside the popup, so the notice is visible
        even when the popup covers the status bar.
*   **Preview pane.**
    *   **Title and note:** the file name is the pane's title. `note` is shown
        dimmed in the pane's bottom border.
    *   **Long lines** are clipped. Rendered markdown is the only content that
        wraps.
    *   **Scrolling:** `Ctrl+F` and `Ctrl+B` scroll by half the pane's inner
        height. The scroll position resets when the selection changes.
    *   **When a preview is requested again:**
        *   the selection changes;
        *   the preview pane appears again after a resize;
        *   the pane width changes while a notebook is shown;
        *   the pane is visible and nothing is shown yet.
*   **Empty states.**
    *   **No files at all:** `no files under <root> — Esc, then Ctrl+P → New
        notebook`. Inside the picker, `Ctrl+P` moves the selection, so the
        picker has to be closed first.
    *   **The scan thread could not start:** `cannot start scan`.
    *   **No matches, with entries hidden by the toggles:**
        `no matches — <k> ignored not shown (M-i)`, where `k` is the number of
        ignored entries that match the query.

### Keys

| Key | Action |
|---|---|
| any printable character, including `j` and `k` | edit the query |
| `Backspace` | delete the last query character |
| `↑` / `↓`, `Ctrl+P` / `Ctrl+N`, `Ctrl+K` / `Ctrl+J`, `Tab` / `Shift+Tab` | move the selection by one row |
| `PgUp` / `PgDn` | move the selection by the number of visible list rows |
| `Enter` | `.ipynb`: open it. Any other file: show the notice and stay open. |
| `Esc` | close the picker |
| `Ctrl+U` | clear the query |
| `Ctrl+F` / `Ctrl+B` | scroll the preview down / up by half a pane |
| `Alt+H` (also `Ctrl+H`) | toggle hidden files (starts a new scan; keeps the query) |
| `Alt+I` | toggle ignored files (refilters; no new scan) |
| paste | appended to the query, trimmed at both ends |

*   **No conflicts.** Global keys are already blocked while an overlay is open
    (`src/app.rs:452`), so none of these clash with notebook keys.
*   **`Ctrl+H`** is kept because the current browser uses it.
*   **`Alt` on macOS.** `Alt` keys need the terminal to send Option as Meta.
    Ghostty does this with `macos-option-as-alt`. Without it:
    *   `Ctrl+H` still toggles hidden files;
    *   the `show_ignored` setting in `config.toml` sets the ignored filter.

### Opening a notebook

`Enter` on a notebook returns `FileAction::Open(path)`. `App` then does one of
the following:

*   **No unsaved changes:** it closes the picker and calls
    `open_notebook(path)`.
*   **Unsaved changes:** it closes the picker and shows
    `ConfirmKind::OpenDiscard { path, reopen: Some(start) }`.
    *   **`y`, `Y` or `Enter`:** the notebook is opened.
    *   **`n`, `N`, `Esc` or `q`:** a fresh picker opens at the same `start`,
        with `path` preselected. The query is lost, and the scan runs again.
        This takes about 100 ms at the target scale.

**Fixing a bug in the existing confirm.** Today, `OpenDiscard` and
`ReloadDiscard` set `nb.dirty = false` before they call `open_notebook`
(`src/app.rs:916` and `:932`).

*   `load_notebook` replaces `self.nb` only when the load succeeds, and that
    replacement resets `dirty` anyway.
*   So when the load fails, the old notebook stays on screen but is marked
    clean, and quitting no longer warns about its edits.
*   The picker makes this easy to hit: preview a file, delete it from another
    pane, then press `Enter`.

Both lines are deleted. A failed load then shows the existing error flash, and
the edits stay marked unsaved.

**Kernel input.** When a running cell asks for input, the kernel's stdin prompt
replaces whatever overlay is open (`src/app.rs:1316`), including the picker.
The picker is then dropped, which stops its workers.

### Config, logging and syntax set

*   **Config:**
    *   **`show_hidden`** (existing) keeps its name. It now covers dot-folders
        as well as dot-files. It is the starting value of the hidden toggle.
    *   **`show_ignored`** (new, default `false`) is the starting value of the
        ignored toggle.
    *   **`icons`** (new, default `true`) turns the Nerd Font icon column on or
        off.
    *   The toggles last for the session. They are not written back to
        `config.toml`.
*   **Logging:**
    *   one `info!` per scan, with the root, the toggles, entries kept, entries
        listed by default, the caps hit, the unreadable count and the elapsed
        time;
    *   one `debug!` per preview build, with the kind, bytes read, whether the
        cache was hit and the elapsed time;
    *   the picker counts the stale events it drops and logs the count with
        `debug!` when it closes.
*   **Syntax set.** A new `src/syntax.rs` holds one
    `SYNTAX_SET: LazyLock<Arc<SyntaxSet>>`, built from
    `two_face::syntax::extra_newlines()`. two-face ships the syntax set of the
    `bat` tool.
    *   **Why the switch:** edtui's default syntect set has no TOML,
        TypeScript or Dockerfile syntax. So `Cargo.toml`, `pyproject.toml` and
        saturn's own `config.toml` would preview as plain text.
    *   **Why app-wide:** one set used by the preview, the cell view and the
        cell editor is simpler than two sets with lookups that can disagree.
    *   **Cost:** two-face scopes some tokens differently. In Python, `def` and
        `class` get an extra `keyword.declaration.*` scope, which would change
        their colour. Commit 2 adds those scopes to the `keyword.function`
        theme key, so they keep their colour (Detailed Plan, item 5a). Other
        tokens may still look slightly different. The swap is its own commit,
        so it can be reverted on its own.
    *   **One syntect build:** two-face 0.5.2 needs syntect 5.3.0 with the onig
        regex engine. That is the same version and engine as edtui's copy in
        `Cargo.lock`.

## Risks and Mitigations

*   **saturn's gitignore results differ from git's.** The cause would be an edge
    case such as `!` rules across folder levels, or an unusual
    `core.excludesFile`.
    *   *Handled by:* the stated precedence and inheritance rules, and an
        optional comparison test against `git check-ignore`. See Ignore rules,
        and the tests for `src/files/walk.rs` (item 14).
*   **A hung file system** (a stalled mount, a cloud placeholder file, or a
    macOS privacy prompt).
    *   *Handled by:* the workers run on detached threads, the UI thread does no
        file I/O, and exit does not wait for the workers. See Components and
        data flow, and Preview worker.
*   **The cost of scanning `~`.**
    *   *Handled by:* pruning hidden folders, the skip list, and the three caps.
        See Scanner.
*   **A file sends escape codes to the terminal.**
    *   *Handled by:* `clean_text` and `clean_name` on every displayed string.
        See Display safety.
*   **Cells look different after the two-face swap.**
    *   *Handled by:* a separate commit and a before/after check of
        `assets/sample.ipynb`. See Verification Plan, step 10.
*   **`Alt` keys don't reach saturn.**
    *   *Handled by:* the `Ctrl+H` fallback, the `show_ignored` setting and a
        README note. See Keys.
*   **Symlinked folders are no longer entered.** The current browser follows
    them.
    *   *Handled by:* a stated non-goal and the `[folder]` placeholder.
*   **No Nerd Font is installed.** Every row then starts with an empty box.
    *   *Handled by:* `icons = false`, and a README note.

## Alternatives Considered

### Alternative A: Keep the one-folder browser and add a preview pane

*   **Pros**: The smallest change. The browser, its keys and the
    `[ new notebook here ]` row stay as they are. No scanner and no ignore
    rules.
*   **Cons**: It fixes only half the problem. The user still walks folder by
    folder.
*   **Reason for Rejection**: "No overview" is a gap of its own, and a preview
    does not address it.

### Alternative B: List only `.ipynb` files, recursively

*   **Pros**:
    *   `Enter` does something on every row.
    *   No text preview, no binary or special-file handling, and no icons per
        file type. About a third of the design goes away.
*   **Cons**: The list no longer shows the project as it is. The `.py` modules,
    configs and data files next to a notebook stay invisible, and these are
    often what the user needs to see to pick the right notebook.
*   **Reason for Rejection**: Seeing the project as it is was part of the
    problem.

### Alternative C: Ship in two steps (list first, preview second)

*   **Pros**: Step one fixes "no overview" sooner, and its caps, skip list and
    keys can be tuned before the preview exists.
*   **Cons**: Two releases of a changing UI. The first has a large popup with
    no preview pane.
*   **Reason for Rejection**: The preview is half the problem statement. The
    commit order still lets the scanner and the preview be built and tested on
    their own.

### Alternative D: Root at the open notebook's folder

*   **Pros**: Matches today's `Ctrl+O`. Smaller scans, and no `.git` lookup.
*   **Cons**: A notebook in `proj/notebooks/exp1/` hides its sibling folders,
    which is the overview problem itself.
*   **Reason for Rejection**: The git root gives the overview, and it matches
    the LazyVim files picker.

### Alternative E: Root navigation keys inside the picker

*   **Mechanism**: `Ctrl+L` sets the root to the selected file's folder.
    `Backspace` on an empty query goes up one folder. `~` goes home, and
    `Ctrl+G` goes back to the starting root.
*   **Pros**: Lets the user narrow a scan of `~`, or look above the repository,
    without restarting saturn.
*   **Cons**:
    *   No goal or CUJ needs it.
    *   It brings back parent-`.gitignore` seeding.
    *   It adds a rescan for each root change.
    *   `Backspace`-up needs a timing guard. The guard first drafted was wrong:
        a held key still climbed one folder every 300 ms.
*   **Reason for Rejection**: Its cost is out of proportion to its use. To list
    another folder, start saturn there.

### Alternative F: Let the `ignore` crate do all gitignore matching (two walks)

*   **Mechanism**:
    *   **Walk 1** uses the crate's own filters (gitignore, global excludes,
        `info/exclude`, worktrees, hidden) and fills the list.
    *   **Walk 2** runs with gitignore off and hidden still pruned. It adds the
        `.ipynb` files walk 1 did not see. With "show ignored" on, it adds
        everything.
*   **Pros**:
    *   saturn has no gitignore logic of its own.
    *   Matching agrees with ripgrep's, and the crate maintains it.
    *   There is no `.git` file parsing.
*   **Cons**:
    *   About twice the scan time, since walk 2 re-reads every folder walk 1
        visited.
    *   It needs a set of the paths walk 1 saw.
    *   The ignored toggle needs a new scan.
*   **Reason for Rejection**: One walk keeps the ignored toggle instant, and it
    reads each folder once. That matters most for the large roots where scan
    time is already the main cost. The price is about 150 lines of matching
    code that must agree with git, which Risks and Mitigations tracks. This
    trade-off was checked again after hidden pruning made the hidden toggle a
    rescan, and the decision held.

### Alternative G: Ask git (`git ls-files`) inside repositories

*   **Mechanism**:
    *   `git ls-files --cached --others --exclude-standard` for the visible
        list.
    *   `git ls-files --others --ignored --exclude-standard` for ignored files.
    *   An `ignore` walk outside repositories.
*   **Pros**: Exact git semantics, and a fast visible list from the index.
*   **Cons**:
    *   saturn would depend on the `git` binary and on subprocesses.
    *   It needs two code paths.
    *   git can refuse to run (a "dubious ownership" error, or an old
        version), and the fallback walk would hide that failure.
*   **Reason for Rejection**: It adds an external dependency and a second code
    path to avoid matching code that the `ignore` crate's `Gitignore` type
    mostly provides.

### Alternative H: Build previews during the draw, on the UI thread

*   **Pros**: About half the code of a worker. The preview always matches the
    selection on the same frame.
*   **Cons**: Every preview costs frame time. A read that blocks freezes the
    whole app: a cloud placeholder file, a stalled network mount, or a slow
    USB disk.
*   **Reason for Rejection**: It breaks G5.

### Alternative I: Cache previews in the picker, keyed by file metadata

*   **Mechanism**: The picker looks up `(path, mtime, len)` before it sends a
    request, and skips the request on a hit.
*   **Cons**: The lookup needs a `stat` on the UI thread. That is the same
    freeze risk as Alternative H. Storing `mtime` and `len` in `FileEntry`
    instead would add one `lstat` per visited entry, up to 1,000,000 visits.
*   **Reason for Rejection**: Putting the cache inside the worker gives the
    same fast return to recently viewed files with no file-system work on the
    UI thread.

### Alternative J: Keep the picker in memory while the discard confirm is shown

*   **Mechanism**: Move the picker into an `App::picker_stash` and put it back
    on `n`.
*   **Pros**: The query, the list and the preview survive a "no".
*   **Cons**:
    *   The app needs a second place to route scan and preview events.
    *   Every way of dismissing the confirm and the prompt needs a restore
        hook.
    *   A stash left behind by one path can come back on an unrelated confirm.
*   **Reason for Rejection**: A fresh picker with the chosen file preselected
    covers CUJ 3. Losing only the query is an acceptable cost.

### Alternative K: A synchronous scan with a hard cap

*   **Pros**: No worker, no event protocol and no cancel flag.
*   **Cons**: A start in `~` blocks the first frame until the cap is hit.
    macOS privacy prompts can block it for longer.
*   **Reason for Rejection**: It breaks G5 and CUJ 6.

### Alternative L: Add a files kind to the command palette (`PaletteState`)

*   **Pros**: Reuses the palette's drawing, keys and filtering.
*   **Cons**:
    *   `PaletteState` holds a fixed item list, so results cannot stream in.
    *   It matches on label plus detail.
    *   It has one pane of at most 18 rows.
    *   It already needs special cases in its key handling for the current
        file browser (`src/ui/overlay.rs:169-185`), and a second files mode
        would add more.
*   **Reason for Rejection**: The `gt` and `z` overlays show the pattern of one
    module per overlay.

### Alternative M: Reuse `cell::render` and `nbio::load` for notebook previews

*   **Pros**: The preview would look exactly like the notebook view.
*   **Cons**:
    *   `cell::render` needs a live `&mut Cell` and the app's render, image and
        plotly caches.
    *   The render cache is keyed by cell id alone, so previewing the open
        notebook would corrupt that cache.
    *   `nbio::load` builds every output.
*   **Reason for Rejection**: The preview would pay for, and risk, parts it
    throws away.

### Alternative N: The `nucleo` crate (streaming matcher)

*   **Pros**:
    *   Its `Injector` and snapshot handle results that stream in.
    *   It is what helix's file picker uses.
*   **Cons**:
    *   A second matcher crate next to `nucleo-matcher`.
    *   Its own worker threads.
    *   A snapshot API that differs from the palette's.
*   **Reason for Rejection**: Rebuilding the filtered list once per frame is
    cheap at the target scale, and it needs no merging logic.

### Alternative O: One toggle for hidden and ignored files together

*   **Cons**:
    *   Someone who wants to see `.env` does not want `node_modules` or a
        gitignored `data/` folder.
    *   One toggle would also change the meaning of the existing `show_hidden`
        key.
*   **Reason for Rejection**: These are different needs, and snacks also gives
    each its own key.

--------------------------------------------------------------------------------

## Non-Functional Requirements

*   **Performance:** see "How success is checked" under Goals.
*   **Memory:**
    *   At most 100,000 kept entries (`SHOWN_CAP` + `EXTRA_CAP`). Each entry is
        about 150–250 bytes for ASCII paths, so 15–25 MB at worst, and well
        under 1 MB at the target scale.
    *   The worker's cache holds at most 32 previews of at most 2,000 lines
        each.
*   **Safety of displayed text:** see Display safety.
*   **Writes:** the picker never writes a file.
*   **Rollout and rollback:**
    *   **Commits 1–7** add code without changing what the user sees, except
        that commit 2 changes highlighting and commit 1 fixes the discard
        bug.
    *   **Commit 8** is the switch. Reverting commit 8 on its own brings the
        old browser back.
    *   **Config:** the new keys have defaults. Config loading already
        tolerates missing keys (`#[serde(default)]`, `src/config.rs:13`).
        However, an invalid value for a *known* key, such as `icons = "yes"`,
        makes the whole config fall back to its defaults
        (`src/config.rs:85-90`). That is current behaviour, and it is
        documented in the README.

## Additional Resources

*   The brainstorm brief and its six-lens review: `concepts_brief.md`, at the
    repository root. It is not committed.
*   The review reports for this design, at the repository root:
    *   `consolidated_review_report.md`, with the decisions D1–D8 that shaped
        this revision;
    *   one report for each review stage.
*   [snacks.nvim picker](https://github.com/folke/snacks.nvim/blob/main/docs/picker.md):
    the reference behaviour.
*   `src/ui/cell_picker.rs` and `src/ui/popout.rs`: the overlay pattern this
    design follows.

## Future Improvements

*   **Partial preview of large text files:** show the first 256 KB with a
    "truncated" marker, as snacks does.
*   **Open non-notebook files:** hand them to `$EDITOR`, or show them in a
    read-only viewer.
*   **Watch the file system,** so the list updates when files are created or
    deleted.
*   **Change the root from inside the picker** (Alternative E), if starting
    saturn elsewhere proves too slow in practice.
*   **Notebooks first** on an empty query, or a "notebooks only" toggle.
*   **A content-search (grep) picker** that reuses this overlay, its workers
    and the preview.

--------------------------------------------------------------------------------

## Detailed Implementation Plan

All paths are relative to the repository root. The work is split into eight
commits, applied in this order.

*   **Each commit registers** the modules it creates.
*   **No commit uses** a type that a later commit adds.
*   **Each commit passes** the build, test and lint gate in the Verification
    Plan.

| # | Commit | Files |
|---|---|---|
| 1 | Fix the discard-confirm data loss | `src/app.rs` |
| 2 | Shared two-face syntax set | `Cargo.toml`, `Cargo.lock`, `src/syntax.rs`, `src/lib.rs`, `src/ui/text.rs`, `src/ui/cell.rs`, `src/theme.rs` |
| 3 | Shared fuzzy helper | `src/ui/fuzzy.rs`, `src/ui/mod.rs`, `src/ui/overlay.rs`, `src/ui/cell_picker.rs` |
| 4 | Text helpers | `src/ui/text.rs`, `src/ui/cell_picker.rs` |
| 5 | Scanner | `Cargo.toml`, `Cargo.lock`, `src/lib.rs`, `src/files/mod.rs`, `src/files/root.rs`, `src/files/skip.rs`, `src/files/walk.rs`, `src/app.rs` |
| 6 | Preview | `src/files/mod.rs`, `src/files/preview.rs`, `src/files/outline.rs`, `src/syntax.rs` |
| 7 | Picker state and drawing (not yet reachable) | `src/ui/file_picker.rs`, `src/ui/icons.rs`, `src/ui/mod.rs`, `src/config.rs` |
| 8 | Switch `Ctrl+O` and startup to the picker; remove the old browser | `src/app.rs`, `src/ui/overlay.rs`, `src/ui/mod.rs`, `src/main.rs`, `src/commands.rs`, `README.md` |

### Target Files to Create/Modify

#### 1. `src/app.rs` — commit 1 (Modify)

*   **Type / Scope**: Bug fix.
*   **Proposed Changes**:
    *   In `confirm`, delete `self.nb.dirty = false;` from both the
        `ConfirmKind::OpenDiscard` arm (line 916) and the
        `ConfirmKind::ReloadDiscard` arm (line 932).
    *   `load_notebook` swaps `self.nb` only on success, and the new
        `Notebook` starts clean. So success behaves as before, and a failed
        load keeps the old notebook's edits marked unsaved.
    *   **Test:** none automated. `App::new` needs a terminal and a runtime.
        Covered by verification step 7.

#### 2. `Cargo.toml`, `Cargo.lock` — commits 2 and 5 (Modify)

*   **Type / Scope**: Build configuration.
*   **Proposed Changes**:
    *   **Commit 2:** add `two-face = "0.5.2"` with its default features
        (`syntect-onig`).
    *   **Commit 5:** add `ignore = "0.4.34"`.
    *   `Cargo.lock` is updated by the build in both commits.
    *   The single-syntect check is in the Verification Plan.

#### 3. `src/syntax.rs` — commit 2 (Create); commit 6 (Modify)

*   **Type / Scope**: The app's single syntax set.
*   **Proposed Changes**:
    *   **Commit 2:**
        *   `pub static SYNTAX_SET: LazyLock<Arc<SyntaxSet>>`, built from
            `two_face::syntax::extra_newlines()`.
        *   `pub fn by_extension(ext: &str) -> Option<&'static SyntaxReference>`.
    *   **Commit 6:**
        *   `pub fn for_file(name: &str, first_line: Option<&str>) ->
            &'static SyntaxReference`.
        *   It tries, in order:
            1.  `find_syntax_by_extension` on the extension;
            2.  the same call on the whole file name (`Makefile`,
                `Dockerfile`);
            3.  `find_syntax_by_first_line`;
            4.  `find_syntax_plain_text()`.
        *   `for_file` never reads the file. The caller supplies `first_line`.

#### 4. `src/lib.rs` — commits 2 and 5 (Modify)

*   **Type / Scope**: Crate root.
*   **Proposed Changes**:
    *   **Commit 2:** `pub mod syntax;`
    *   **Commit 5:** `pub mod files;`

#### 5. `src/ui/text.rs` — commits 2 and 4 (Modify)

*   **Type / Scope**: Text helpers.
*   **Proposed Changes**:
    *   **Commit 2:** in `highlight`, replace `&*edtui::SYNTAX_SET` with
        `&*crate::syntax::SYNTAX_SET`.
    *   **Commit 4, `highlight_with`:**
        *   Add `pub fn highlight_with(source: &str, syntax: &SyntaxReference,
            theme: &SynTheme, tab_width: usize) -> Text<'static>`, holding the
            body that `highlight` has today.
        *   `highlight(source, ext, …)` calls `by_extension(ext)` and passes the
            result to `highlight_with`. With no syntax, it returns its current
            plain-line output.
    *   **Commit 4, cleaning:**
        *   `pub fn clean_text(s: &str) -> Cow<'_, str>` and `pub fn
            clean_name(s: &str) -> Cow<'_, str>`, exactly as in Display safety.
        *   Both return `Borrowed` when nothing changes.
    *   **Commit 4, cutting:**
        *   Move `cut(s, width)` from `cell_picker.rs` here as `pub`. It is
            width-aware: it keeps the start of the string and adds `…`, filling
            the result to exactly `width` columns.
        *   Add a mirrored `pub fn cut_left(s, width)`. It keeps the end of the
            string, with a leading `…`, filling the result to exactly `width`
            columns.
        *   A list row uses `cut_left` on the whole `display`. When the file
            name alone is wider than the row, the row instead shows the file
            name cut with `cut`, and no folder part.
    *   **Tests:**
        *   `clean_text` on: a CSI colour sequence; an OSC 8 hyperlink ended by
            `ESC \`; an OSC ended by BEL; an unterminated CSI; `ESC c`; a lone
            `\r`; `\r\n` (becomes `\n`); a tab (kept); DEL; a bidi override.
        *   `clean_name` on an input with one character from each class: tab,
            `\r`, `\n`, another C0 control, DEL, a C1 control, ESC and a bidi
            override. Each becomes `?`, and the output has the same number of
            characters as the input.
        *   A snapshot of `highlight("def f(x):\n    return x\n", "py", …)`.
            It is taken before commit 4 and must be unchanged after it.
        *   `cut_left("abc/def.py", 8) == "…/def.py"`.
        *   `cut_keeps_width` moves here unchanged.

#### 5a. `src/theme.rs` — commit 2 (Modify)

*   **Type / Scope**: Semantic syntax keys.
*   **Proposed Changes**:
    *   **The problem.** two-face's Python syntax adds a deeper scope to two
        keywords, and syntect's default set does not:
        *   `def` gets `keyword.declaration.function.python`;
        *   `class` gets `keyword.declaration.class.python`.

        A scratch build comparing the two sets confirmed this. Without a
        change, the plain `keyword` key would match that deeper scope and win
        over `keyword.function`, which matches only `storage.type.function`.
        So `def` and `class` would turn from the function-keyword colour to the
        keyword colour in every theme.
    *   **The fix.** In the `SEMANTIC` table (`src/theme.rs:49`), extend the
        `keyword.function` selectors to `storage.type.function,
        storage.type.class, storage.modifier.async,
        keyword.declaration.function, keyword.declaration.class`.
    *   **Test:** `python_highlighting_uses_semantic_keys`
        (`src/theme.rs:991`) must pass unchanged after commit 2. It expects
        `def` to be `#66d9ef` in monokai.

#### 6. `src/ui/cell.rs` — commit 2 (Modify)

*   **Type / Scope**: Cell rendering.
*   **Proposed Changes**: At lines 187–192, look the syntax up in
    `crate::syntax::SYNTAX_SET`, and pass that `Arc` to
    `SyntaxHighlighter::with_sets` in place of `edtui::SYNTAX_SET`.

#### 7. `src/ui/fuzzy.rs` — commit 3 (Create)

*   **Type / Scope**: Shared fuzzy matching.
*   **Proposed Changes**:
    *   **`pub struct Fuzzy { matcher: Matcher, pattern: Option<Pattern> }`.**
        Callers keep their haystacks as `Utf32String` (built once per item)
        and pass `h.slice(..)`. `Fuzzy` holds no scratch buffer.
    *   **`pub fn new(paths: bool) -> Self`** uses
        `MatcherConfig::DEFAULT`, plus `.match_paths()` when `paths` is true.
    *   **`pub fn set_query(&mut self, q: &str)`** uses
        `Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart)`. An
        empty `q` sets the pattern to `None`.
    *   **`pub fn score(&mut self, hay: Utf32Str<'_>) -> Option<u32>`.** With
        no pattern, every haystack scores `Some(0)`.
    *   **`pub fn indices(&mut self, hay: Utf32Str<'_>, out: &mut Vec<u32>)`**
        clears `out`, fills it, then sorts and dedups it.
    *   **`pub fn rank<'a, I>(&mut self, items: I) -> Vec<u32>`**, where `I:
        Iterator<Item = (u32, Utf32Str<'a>, u32)>`. Each item is (index,
        haystack, tie-break length). It drops non-matches, then sorts by:
        1.  score, highest first;
        2.  tie-break length, shortest first;
        3.  index.
    *   **Tests:**
        *   an empty query keeps the input order;
        *   a non-match is dropped;
        *   equal scores with length 0 keep index order;
        *   equal scores with lengths 5 and 3 put the 3 first;
        *   query `config` ranks `config.py` above `c/o/n/f/i/g.txt`;
        *   `indices` come back sorted and unique.

#### 8. `src/ui/mod.rs` — commits 3, 7 and 8 (Modify)

*   **Type / Scope**: Module list and drawing entry point.
*   **Proposed Changes**:
    *   **Commit 3:** `pub mod fuzzy;`
    *   **Commit 7:** `pub mod file_picker; pub mod icons;`
    *   **Commit 8:** add an `Overlay::Files(p)` branch to `draw`, before the
        notebook is drawn and next to the `Popout` branch. It draws the status
        bar, then `file_picker::render(buf, main, p, &app.theme)`, and
        returns.

#### 9. `src/ui/overlay.rs` — commits 3 and 8 (Modify)

*   **Type / Scope**: Overlay types, palette and help.
*   **Proposed Changes**:
    *   **Commit 3:** `PaletteState`'s `matcher: Matcher` field becomes
        `fuzzy: Fuzzy` (`Fuzzy::new(false)`).
        *   `filter` builds one `Utf32String` per item from the same text as
            today (`label` plus `detail`).
        *   It calls `rank` with a tie-break length of 0.
        *   It casts the `Vec<u32>` result to `Vec<usize>`.

        Results do not change.
    *   **Commit 8, add:**
        *   **The variant** `Overlay::Files(Box<FilePickerState>)`. It is boxed
            so it does not trip `clippy::large_enum_variant`.
        *   **In `render`,** an `Overlay::Files(_)` arm that draws nothing.
        *   **`pub fn paste(&mut self, text: &str)`:**
            *   `Palette`, `Cells` and `Files` append `text.trim()` to their
                query and refilter.
            *   `Prompt` (the password prompt included) appends `text` with
                trailing `\r` and `\n` removed.
            *   Other overlays ignore the paste.

            This matches the trimming in `src/app.rs:262-270` today.
    *   **Commit 8, remove:**
        *   `PaletteKind::Files`;
        *   `ItemData::Dir`, `ItemData::File` and `ItemData::NewNotebook`;
        *   `PaletteAction::Parent`, `PaletteAction::Home` and
            `PaletteAction::ToggleHidden`;
        *   the `Ctrl+H`, `~` and `-` key arms, and the `Files` branch of the
            `Backspace` arm, in `PaletteState::handle_key`. `Backspace` then
            only edits the query.
    *   **Commit 8, text:** the module doc drops "files". The `HELP` line
        `Ctrl+O   open file / browse` becomes `Ctrl+O   find file`.

#### 10. `src/ui/cell_picker.rs` — commits 3 and 4 (Modify)

*   **Type / Scope**: The `gt` picker.
*   **Proposed Changes**:
    *   **Commit 3:** `CellPickerState`'s `matcher: Matcher` field becomes
        `fuzzy: Fuzzy` (`Fuzzy::new(false)`).
        *   `filter` builds one `Utf32String` per row from the same text as
            today (`source`, plus the output excerpt).
        *   It calls `rank` with a tie-break length of 0, and casts the result
            to `Vec<usize>`.
        *   The numeric-query branch and the order stay as they are.
    *   **Commit 4:** use `ui::text::cut` in place of the local copy.
        `cut_keeps_width` moves to `text.rs` unchanged. The other existing
        tests stay here and pass unchanged.

#### 11. `src/files/mod.rs` — commit 5 (Create); commit 6 (Modify)

*   **Type / Scope**: Shared types.
*   **Proposed Changes**:
    *   **Commit 5, types.** All fields are `pub`, so `walk.rs`,
        `file_picker.rs` and `app.rs` can read them.
        *   `FileEntry`, as in the Technical Plan. Derives: `Clone, Debug`.
        *   `PickerPrefs { show_hidden: bool, show_ignored: bool }`. Derives:
            `Clone, Copy, Debug, Default, PartialEq`.
        *   `ScanLimits { shown: u32, extra: u32, visit: u32 }`. Derives:
            `Clone, Copy, Debug`. It implements `Default` by hand, giving
            `SHOWN_CAP`, `EXTRA_CAP` and `VISIT_CAP`.
        *   `ScanSummary`, with derives `Clone, Debug, Default` and these
            fields:
            *   `shown: u32`
            *   `kept_extra: u32`
            *   `dropped_extra: u32`
            *   `visited: u32`
            *   `unreadable: u32`
            *   `hit_shown_cap: bool`
            *   `hit_extra_cap: bool`
            *   `hit_visit_cap: bool`
            *   `elapsed: Duration`
        *   `FilesEvent` with `Root { scan_id, root, open_path }`,
            `Batch { scan_id, entries }` and `Done { scan_id, summary }`.
        *   `pub type Sink = Arc<dyn Fn(FilesEvent) + Send + Sync>`.
    *   **Commit 5, ids:** `pub fn next_id() -> u64`, backed by a `static
        NEXT_ID: AtomicU64`. It supplies both `scan_id` and the preview `id`.
        The field is not called `gen`, because `gen` is a reserved keyword in
        Rust 2024, which this crate uses (`Cargo.toml:4`).
    *   **Commit 5, helpers:** move `human_size` and `shorten_path` here as
        `pub`, unchanged.
        *   Delete them from `src/app.rs` and import them there in the same
            commit. That is the one `app.rs` change in commit 5.
    *   **Commit 5, test helper:**
        *   `#[cfg(test)] pub(crate) fn scratch_dir(tag: &str) -> PathBuf`
            creates a fresh folder under `std::env::temp_dir()`, named after
            the process id and `tag`.
        *   It follows `src/config.rs:140`. No new dev-dependency is needed.
    *   **Commit 6:** add `FilesEvent::Preview { id: u64, preview: Preview }`.

#### 12. `src/files/root.rs` — commit 5 (Create)

*   **Type / Scope**: Root discovery.
*   **Proposed Changes**:
    *   **Types**, with `pub` fields and derives `Clone, Debug, PartialEq`:
        *   `pub struct RootInfo { pub root: PathBuf, pub repo: Option<Repo> }`.
        *   `pub struct Repo { pub work_tree: PathBuf, pub common_dir: PathBuf }`.
            `work_tree` is the folder that contains `.git`.
    *   **`pub fn find_root(start: &Path) -> RootInfo`:**
        1.  Canonicalize `start`. If that fails, use it as given.
        2.  Walk up the parents to the first folder that holds a `.git`
            entry.
            *   **`.git` is a folder:** `common_dir` is that folder.
            *   **`.git` is a file:** read its `gitdir: <path>` line.
                *   Trim the path, and resolve it against the folder that holds
                    the `.git` file.
                *   If that gitdir holds a `commondir` file, trim its contents
                    and resolve them against the gitdir. The result is
                    `common_dir`. Otherwise `common_dir` is the gitdir.
            *   A `.git` file that cannot be read or parsed is skipped, and the
                walk goes on upwards.
        3.  **Repository found:** `root` = `work_tree`.
        4.  **No repository:** `root` = the canonical `start` and `repo` =
            `None`.
    *   **Tests.** Every expected path is canonicalized before it is compared.
        On macOS, `temp_dir()` resolves to `/private/var/…`.
        *   a folder holding `.git/`;
        *   a nested folder inside one;
        *   a worktree-style `.git` file whose gitdir holds a relative
            `commondir` (`../..`);
        *   no repository;
        *   a `.git` file holding junk.

#### 13. `src/files/skip.rs` — commit 5 (Create)

*   **Type / Scope**: The built-in skip list.
*   **Proposed Changes**:
    *   **`pub fn prune(path: &Path, name: &OsStr, is_dir: bool, depth: usize,
        prefs: PickerPrefs, home: Option<&Path>) -> bool`.** It returns true
        when any of these holds:
        *   `name == ".git"` (file or folder);
        *   `depth > 0`, `!prefs.show_hidden` and `name` starts with `.`;
        *   `is_dir` and `name` is one of `node_modules`, `.venv`, `venv`,
            `__pycache__`, `.ipynb_checkpoints`, `site-packages` or `.tox`;
        *   `is_dir` and `path` equals `<home>/Library` or `<home>/.Trash`;
        *   `is_dir`, and the folder holds an entry named `CACHEDIR.TAG` or
            `pyvenv.cfg`. Each name is checked with one `symlink_metadata`
            call.
    *   **Tests:**
        *   each listed name;
        *   a `.git` file;
        *   a folder holding `CACHEDIR.TAG`;
        *   a venv with an unusual name that holds `pyvenv.cfg`;
        *   a plain `target/` folder without the tag is kept;
        *   a dot-name is pruned at depth 1 but not at depth 0;
        *   with `show_hidden`, a dot-name is kept.

#### 14. `src/files/walk.rs` — commit 5 (Create)

*   **Type / Scope**: Scanner.
*   **Proposed Changes**:
    *   **Constants:**
        *   `pub const SHOWN_CAP: u32 = 50_000;`
        *   `pub const EXTRA_CAP: u32 = 50_000;`
        *   `pub const VISIT_CAP: u32 = 1_000_000;`
        *   `const BATCH_LEN: usize = 512;`
        *   `const BATCH_EVERY: Duration = Duration::from_millis(30);`
    *   **`pub enum GlobalExcludes { Load, Skip, File(PathBuf) }`**, which
        derives `Clone, Debug`.
        *   `Load` (used by the app) reads `core.excludesFile` through
            `GitignoreBuilder::new(work_tree).build_global()`.
        *   `Skip` (used by most tests) loads nothing.
        *   `File(path)` (used by one test) loads that file with
            `GitignoreBuilder::new(work_tree)`, then `add(path)`, then
            `build()`.
    *   **`pub struct ScanRequest`**, with `pub` fields and derives
        `Clone, Debug`:
        *   `scan_id: u64`
        *   `start: PathBuf`
        *   `open_path: Option<PathBuf>`
        *   `prefs: PickerPrefs`
        *   `limits: ScanLimits`
        *   `home: Option<PathBuf>`
        *   `global: GlobalExcludes`
        *   `cancel: Arc<AtomicBool>`
    *   **`pub fn spawn_scan(req: ScanRequest, sink: Sink)`** starts a detached
        `std::thread` named `saturn-scan`, which runs `scan(req, &*sink)`.
        *   If `thread::Builder::spawn` fails, it logs `warn!` and emits
            `Done` straight away, with an empty summary.
        *   The picker then shows `cannot start scan` in place of the empty
            state.
    *   **`pub fn scan(req: ScanRequest, emit: &dyn Fn(FilesEvent))`** is the
        core loop. It reports through a callback, so tests call it directly.
        It runs these steps:
        1.  **Root.**
            *   Call `find_root(&req.start)`.
            *   Canonicalize `open_path`. If that fails, join it to the current
                directory.
            *   Emit `Root`.
        2.  **Base matchers**, inside a repository only:
            *   **`info/exclude`:** `GitignoreBuilder::new(work_tree)`, then
                `add(<common_dir>/info/exclude)` if the file exists, then
                `build()`.
            *   **The global file:** according to `req.global`.
            *   Any `add` or `build` error, and the error half of
                `build_global()`, is logged once with `warn!`. The scan then
                carries on with `Gitignore::empty()` for that matcher.
        3.  **Walker.** `WalkBuilder::new(root)` with:
            *   `standard_filters(false)`
            *   `follow_links(false)`
            *   `same_file_system(true)`
            *   `sort_by_file_name(|a, b| a.cmp(b))`
            *   `filter_entry(|e| !skip::prune(...))`

            Build it with `.build()`, the sequential iterator.
        4.  **Rule stack.** `frames: Vec<Frame>`, where `Frame { depth: usize,
            gi: Option<Gitignore>, ignored: bool }`.
            *   The walker yields the root first, at depth 0.
            *   For the root, the scanner pushes a frame with `gi` from
                `root/.gitignore` and `ignored = false`. It does not treat the
                root as an entry.
            *   The base matchers are not frames, so they are never popped.
        5.  **For each later walker item, `Ok` or `Err`:**
            1.  If `cancel` is set, return without emitting `Done`.
            2.  If `visited == limits.visit`, set `hit_visit_cap` and stop.
                This item is not processed. Otherwise `visited += 1`.
            3.  **An `Err`:** `unreadable += 1`. Keep its path, when it has one,
                among the first five kept for the log. Go to the next item.
            4.  Pop the frames whose `depth >= entry.depth()`. The parent frame
                is then on top.
            5.  **Work out `ignored`:**
                *   If the parent frame is ignored, the entry is ignored.
                *   Otherwise, walk the frames from the top down. For each
                    `gi`, call `matched(path, is_dir)`:
                    *   `Match::Ignore`: ignored;
                    *   `Match::Whitelist`: not ignored;
                    *   `Match::None`: try the next frame.
                *   If no frame decides, try the `info/exclude` matcher, then
                    the global matcher, the same way.
                *   If nothing decides, the entry is not ignored.
            6.  **A folder:** push a frame with `ignored` from step 5. If
                `.gitignore` exists in it, set `gi` from
                `Gitignore::new(dir.join(".gitignore"))`, which returns
                `(Gitignore, Option<Error>)`. Log each file's parse error once,
                with its path.
            7.  **A symlink, or any other non-folder:** build a `FileEntry`.
                *   `display` = `clean_name` applied to the lossy,
                    `/`-joined `path.strip_prefix(root)`.
                *   `hay` = `Utf32String::from(&*display)`.
                *   `name_at` = the byte offset just after the last `/` in
                    `display`, or 0.
                *   `notebook` = the name ends in `.ipynb`.
            8.  **Caps.** An entry is listed by default when
                `!ignored || entry.notebook`.
                *   **Listed by default:** if `shown == limits.shown`, set
                    `hit_shown_cap` and stop. Otherwise keep it, and
                    `shown += 1`.
                *   **Otherwise:** if `kept_extra < limits.extra`, keep it and
                    add one to `kept_extra`. If not, set `hit_extra_cap`, add
                    one to `dropped_extra`, and drop it.
        6.  **Batches.** Emit a `Batch` when it holds `BATCH_LEN` entries, or
            when `BATCH_EVERY` has passed since the last batch was emitted. A
            `Batch` is emitted only when it is not empty.
        7.  **At the end:**
            *   Emit the last non-empty `Batch`, then `Done`.
            *   Log one `info!` with the summary.
            *   If `unreadable > 0`, log one `warn!` with the first five paths.
    *   **Test setup:**
        *   Each test makes a scratch folder.
        *   A test that needs a repository creates `dir.join(".git")` as an
            empty folder.
        *   Tests use `GlobalExcludes::Skip`, unless they test the global file,
            so results never depend on the machine's git config.
        *   Expected paths are compared after canonicalizing.
    *   **Tests:**
        *   **Ignore rules:**
            *   a root `.gitignore` with `data/`: `data/a.csv` is ignored and
                `data/n.ipynb` is listed by default;
            *   a nested `.gitignore` re-includes a file with `!keep.txt`;
            *   a `!` rule inside an ignored parent folder does not re-include,
                as in git;
            *   an `info/exclude` rule;
            *   `info/exclude` found through a worktree `.git` file;
            *   `GlobalExcludes::File` with a fixture file holding `*.tmp`
                ignores `x.tmp`;
            *   outside a repository, a `.gitignore` still applies;
            *   a nested repository's `.gitignore` applies, and its `.git` is
                not entered.
        *   **Hidden files and skip folders:**
            *   dot-files and dot-folders are pruned without `show_hidden`, and
                listed with it;
            *   a scan root named `.dotfiles` lists its files;
            *   skip-list folders are never entered.
        *   **Caps:**
            *   `limits.shown = 3` with exactly three entries sets no cap flag;
            *   `limits.shown = 3` with four entries keeps three and sets
                `hit_shown_cap`;
            *   `limits.extra = 2` with five ignored entries keeps two, and
                sets `dropped_extra = 3` and `hit_extra_cap`.
        *   **Control and order:**
            *   a cancel flag that is already set gives no `Done`;
            *   two runs give the same order;
            *   a symlink loop (`a/link -> ..`) ends, because links are not
                followed.
        *   **Errors:** a `chmod 000` folder counts as unreadable. The test is
            `#[cfg(unix)]`, and is skipped when `libc::geteuid() == 0`, since
            root can read the folder anyway.
        *   **`open_path`:** `Root` carries the canonical `open_path` when given
            the relative path `tests/fixtures/sample.ipynb`. Cargo runs tests
            from the repository root.
    *   **Optional test** (`#[ignore]`, run when `git` is installed): for a
        fixture tree, compare each path's ignored flag with
        `git check-ignore --no-index -q`.
    *   **Non-UTF-8 names.** APFS rejects non-UTF-8 file names, so the
        non-UTF-8 test skips itself on macOS and runs on Linux. It checks that
        `display` is lossy and that `path` keeps the original bytes.

#### 15. `src/files/preview.rs` — commit 6 (Create)

*   **Type / Scope**: Classification, building and the preview worker.
*   **Proposed Changes**:
    *   **Constants:**
        *   `TEXT_MAX: u64 = 256 * 1024`
        *   `NB_MAX: u64 = 32 * 1024 * 1024`
        *   `SNIFF: usize = 8 * 1024`
        *   `MAX_LINES: usize = 1_000`
        *   `MAX_LINE_CHARS: usize = 1_000`
        *   `NB_MAX_LINES: usize = 2_000`
        *   `CACHE_LEN: usize = 32`
    *   **Types**, all with `pub` fields:
        *   `UiColors { muted: Color, input_bg: Option<Color>, line_numbers:
            Color }`, which derives `Clone, Copy, Debug`.
        *   `PreviewRequest`, as in the Technical Plan, which derives
            `Clone, Debug`.
        *   `Preview`, which derives `Clone, Debug`. It has two variants:
            *   `Doc { lines: Vec<Line<'static>>, line_numbers: bool,
                note: Option<String> }`;
            *   `Placeholder(String)`.
        *   `CacheKey { path: PathBuf, mtime: Option<SystemTime>, len: u64,
            width: Option<u16> }`, which derives `Clone, PartialEq, Eq`.
    *   **`pub fn build_path(req: &PreviewRequest) -> (Option<CacheKey>,
        Preview)`:**
        *   It calls `metadata()`. On an error it returns `(None,
            Placeholder(format!("cannot read: {e}")))`.
        *   Otherwise it builds the key, sets `width` for `.ipynb` only, and
            returns `(Some(key), build(req, &meta))`.
    *   **`pub fn build(req: &PreviewRequest, meta: &Metadata) -> Preview`**
        runs rows 2–8 of the classification table, in order.
        *   Placeholder texts are exactly the ones in the table. Sizes come
            from `human_size`.
        *   Special files are detected from `meta.file_type()`
            (`FileTypeExt::is_fifo`, `is_socket`, `is_block_device` and
            `is_char_device`), and are never opened.
        *   The NUL sniff opens the file and reads at most `SNIFF` bytes.
    *   **The text branch:**
        1.  Read with `.take(TEXT_MAX)`, so a file that grew after the size
            check is still bounded.
        2.  Decode with `from_utf8_lossy`, then apply `clean_text`.
        3.  Split on `\n`. A trailing empty segment after a final `\n` is not
            counted as a line.
        4.  Keep the first `MAX_LINES` lines. Cut each one to `MAX_LINE_CHARS`
            characters, never splitting a character.
        5.  Join the lines back with `\n`. Then call
            `syntax::for_file(name, first_line)` and `highlight_with`.
        6.  Set `line_numbers: true`. Set `note` to
            `first {MAX_LINES} of <n> lines` when lines were dropped.
    *   **The worker:** `pub struct PreviewWorker { slot: Arc<Slot> }`, with
        `Slot { state: Mutex<SlotState>, cv: Condvar }` and
        `SlotState { req: Option<PreviewRequest>, closed: bool }`.
        *   **`spawn(sink: Sink) -> Self`** starts a detached `std::thread`
            named `saturn-preview`. The thread owns a cache: a `VecDeque` of
            `CACHE_LEN` `(CacheKey, Preview)` pairs (no new crate). Its loop:
            1.  Lock `state`. Wait with
                `cv.wait_while(guard, |s| s.req.is_none() && !s.closed)`.
            2.  If `closed` is set, return. This ends the thread.
            3.  Take `req`, then drop the lock.
            4.  Call `build_path(&req)` for the key, but check the cache
                before building. On a hit, use the cached preview and move it
                to the front. On a miss, build, then insert at the front, and
                drop the oldest entry if the cache is over `CACHE_LEN`.
            5.  Emit `FilesEvent::Preview { id: req.id, preview }`.
            6.  Log one `debug!`, and continue the loop.
        *   **`request(&self, req)`:** lock, replace `state.req`, unlock, then
            `notify_one`.
        *   **`impl Drop`:** lock, set `state.closed = true`, unlock, then
            `notify_one`. Because `closed` is set under the lock, the worker
            cannot miss the wake-up.
        *   **`#[cfg(test)] fn spawn_counted(sink, builds: Arc<AtomicUsize>)
            -> Self`** is the same, except that it adds one to `builds` on
            every cache miss.
    *   **Tests** that call `build_path` or `build` directly:
        *   **Binary and size:**
            *   a file with a NUL byte gives `binary · <size>`;
            *   a 300 KiB file that starts with the PNG signature bytes, written
                by the test, also gives `binary · 300.0K`;
            *   invalid UTF-8 comes out lossy;
            *   an empty file gives one empty line;
            *   a 300 KiB text file gives `too large to preview (300.0K)`;
            *   a sparse 33 MiB `.ipynb`, made with `File::set_len`, gives
                `notebook too large to preview (33.0M)`;
            *   `tests/fixtures/sample.ipynb` gives a notebook preview, a `Doc`
                with `line_numbers: false`.
        *   **Lines:**
            *   5,000 lines, ending in `\n`, keep 1,000, with the note
                `first 1000 of 5000 lines`;
            *   a CRLF file has no `\r` left, a tab is expanded to `tab_width`
                spaces, and an ESC sequence is gone;
            *   a line of 1,200 CJK characters is cut to 1,000 characters
                without splitting one.
        *   **Special paths:**
            *   a FIFO made with `libc::mkfifo` gives `[fifo]`, checked in a
                thread with a 1 s timeout;
            *   a symlink to a folder gives `[folder]`;
            *   `build_path` on a missing path gives `cannot read: …`.
        *   **Syntax:**
            *   a `#!/usr/bin/env python3` file with no extension gets the
                Python syntax;
            *   `Cargo.toml` gets a TOML syntax.
        *   **Timed (`#[ignore]`;** run with `cargo test --release --
            --ignored`):
            *   a 2 MiB single-line text file is rejected by size in under
                50 ms;
            *   a 200 KiB single-line file is cut to 1,000 characters in under
                50 ms.
    *   **Worker tests**, with a sink that collects events and with
        `spawn_counted`:
        *   **Merging requests:** send two requests, then wait for an event
            with the second id. That event exists, and no event has any other
            id except possibly the first. The test does not assume the first
            was skipped.
        *   **Cache hit:** request the same file twice, waiting for each
            result. `builds` is 1.
        *   **Cache miss:** set the file's mtime 10 s forward with
            `File::set_modified`, then request it again. `builds` is 2.
        *   **Stopping:** dropping the worker ends its thread. The test keeps a
            `JoinHandle` through a `#[cfg(test)]` accessor and joins it within
            1 s.

#### 16. `src/files/outline.rs` — commit 6 (Create)

*   **Type / Scope**: Notebook preview.
*   **Proposed Changes**:
    *   **Structs for serde.** Every struct derives `Deserialize, Default`.
        Unknown fields are skipped, and `outputs` is not declared.

        ```text
        NbFile  { #[serde(default)] cells: Vec<NbCell>, #[serde(default)] metadata: NbMeta, nbformat: Option<u32> }
        NbCell  { #[serde(default)] cell_type: String, #[serde(default)] source: Source }
        Source  = String | Vec<String>        // #[serde(untagged)]; Default = String::new()
        NbMeta  { #[serde(default)] language_info: Option<LangInfo>, #[serde(default)] kernelspec: Option<KernelSpec> }
        LangInfo   { #[serde(default)] name: Option<String> }
        KernelSpec { #[serde(default)] language: Option<String> }
        ```

    *   **`pub fn build(bytes: &[u8], width: u16, syn: &SynTheme, tab_width:
        usize, ui: UiColors) -> Preview`:**
        1.  **Parse.** Use `serde_json::from_slice::<NbFile>`.
            *   A parse error gives `Placeholder("cannot preview: <error>")`.
            *   `nbformat` defaults to 4 when it is missing. A value below 4
                gives `Placeholder("v3 notebook: no preview (saturn can open
                it)")`.
        2.  **Language.** Take the first that is set: `language_info.name`,
            then `kernelspec.language`, then `"python"`. Map it through
            `editor::language_extension`, then `syntax::by_extension`, and fall
            back to plain text.
        3.  **Cells.** Draw them as described in the Technical Plan.
            Markdown is rendered at `width - 1`, which leaves one column free
            so wrapped text never touches the pane border.
        4.  **Limit.** Stop at `NB_MAX_LINES`, and set the note.
        5.  **Gutter.** Set `line_numbers: false`, because the separators
            carry the cell numbers.
    *   **Tests.** The fixtures are `tests/fixtures/sample.ipynb`,
        `jupyterlab.ipynb` and `legacy44.ipynb`.
        *   **Separators:** each fixture gives one separator per cell.
        *   **No outputs:**
            *   For `sample.ipynb` and `jupyterlab.ipynb`, a `const` probe
                string next to the test is absent from the preview. Each probe
                is taken from that fixture's outputs and appears in no cell
                source. The implementer copies the probes from the fixtures
                when writing the test.
            *   For `legacy44.ipynb`, whose only output `hi` also appears in its
                source, the check is that the preview has as many lines as the
                cell sources plus the separators.
        *   **Parsing variants:**
            *   a `source` given as a list;
            *   a notebook whose `kernelspec` has `name` and `display_name`
                but no `language` previews normally;
            *   an empty notebook gives no cells and no added empty cell;
            *   invalid JSON;
            *   `{"nbformat": 3, "worksheets": []}`.
        *   **Timed:** a 20 MiB synthetic notebook with one large base64 output
            parses in under 300 ms. This test is marked `#[ignore]`; run it
            with `cargo test --release -- --ignored`.

#### 17. `src/ui/icons.rs` — commit 7 (Create)

*   **Type / Scope**: File-type icons.
*   **Proposed Changes**:
    *   **`pub fn icon(name: &str, notebook: bool, ui: &Ui) -> (char,
        Color)`.** It checks these in order and uses the first that matches:
        1.  `notebook`;
        2.  the whole file name, where `README` and `LICENSE` match as
            prefixes;
        3.  the lowercased extension;
        4.  the default, which is also the result for any file the table does
            not list.
    *   **The table.** Each code point below was checked against Nerd Fonts'
        `glyphnames.json`, version 3.5.1. The code point is the authority. The
        glyph name is only a label.

        | Match | Glyph name | Code point | Colour field |
        |---|---|---|---|
        | notebook (`.ipynb`) | `nf-md-notebook` | U+F082E | `focus` |
        | `py`, `pyi` | `nf-seti-python` | U+E606 | `info` |
        | `rs` | `nf-dev-rust` | U+E7A8 | `info` |
        | `js` | `nf-seti-javascript` | U+E60C | `info` |
        | `ts` | `nf-seti-typescript` | U+E628 | `info` |
        | `sh`, `zsh`, `bash` | `nf-seti-shell` | U+E691 | `info` |
        | `sql` | `nf-dev-database` | U+E706 | `info` |
        | `r`, `jl`, `html`, `css` | `nf-fa-file_code_o` | U+F1C9 | `info` |
        | `toml`, `ini`, `cfg`, `conf`, `yaml`, `yml`, `json` | `nf-seti-config` | U+E615 | `warning` |
        | `lock` | `nf-fa-lock` | U+F023 | `warning` |
        | `Makefile` | `nf-seti-makefile` | U+E673 | `warning` |
        | `Dockerfile` | `nf-dev-docker` | U+E7B0 | `warning` |
        | `.gitignore`, `.gitattributes` | `nf-dev-git` | U+E702 | `warning` |
        | `csv`, `tsv`, `parquet`, `pkl`, `pickle` | `nf-fa-table` | U+F0CE | `success` |
        | `bz2`, `gz`, `zip`, `xz`, `tar` | `nf-fa-file_archive_o` | U+F1C6 | `success` |
        | `md`, `README*` | `nf-seti-markdown` | U+E609 | `muted` |
        | `txt`, `log`, `LICENSE*` | `nf-fa-file_text_o` | U+F0F6 | `muted` |
        | `png`, `jpg`, `jpeg`, `gif`, `svg` | `nf-fa-file_image_o` | U+F1C5 | `muted` |
        | `pdf` | `nf-fa-file_pdf_o` | U+F1C1 | `muted` |
        | anything else | `nf-fa-file_o` | U+F016 | `muted` |

    *   **The icon slot.** With `icons = true`, the row reserves exactly 2
        columns: the glyph and one space. A glyph that a terminal draws two
        cells wide then fills the slot instead of shifting the text. With
        `icons = false`, `icon` is not called and no slot is reserved.
    *   **Tests:**
        *   every glyph in the table is in the Private Use Area: U+E000–U+F8FF,
            or U+F0000 and above;
        *   `README.md` matches `README*` before `md`;
        *   `Makefile` matches by its whole name;
        *   an unknown extension gets the default.

#### 18. `src/ui/file_picker.rs` — commit 7 (Create)

*   **Type / Scope**: Picker state, keys and drawing.
*   **Proposed Changes**:
    *   **`pub trait PickerIo`**, with two methods: `fn scan(&self, req:
        ScanRequest)` and `fn preview(&self, req: PreviewRequest)`.
        *   **`ThreadIo { sink: Sink, worker: PreviewWorker }`** (the app). It
            calls `spawn_scan` and `worker.request`.
        *   **`#[cfg(test)] RecordIo { calls: Rc<RefCell<Vec<IoCall>>> }`**,
            with `#[cfg(test)] enum IoCall { Scan(ScanRequest),
            Preview(PreviewRequest) }`. It records each call and starts no
            threads. A test clones `calls` before it boxes the `RecordIo`, so
            it can read the calls later.
    *   **`pub struct FilePickerState`.** Its fields are private, apart from
        `pub start: PathBuf`:
        *   **Scan:**
            *   `io: Box<dyn PickerIo>`
            *   `start`
            *   `root: Option<PathBuf>`
            *   `open_path: Option<PathBuf>`
            *   `open_dirty: bool`
            *   `scan_id: u64`
            *   `cancel: Arc<AtomicBool>`
            *   `scanning: bool`
            *   `summary: Option<ScanSummary>`
            *   `scan_failed: bool`
        *   **List:**
            *   `entries: Vec<FileEntry>`
            *   `filtered: Vec<u32>`
            *   `filtered_stale: bool`
            *   `selected: Option<u32>`
            *   `user_moved: bool`
            *   `preselect: Option<PathBuf>`
            *   `list_top: usize`
            *   `list_rows: u16`
        *   **Query:**
            *   `query: String`
            *   `fuzzy: Fuzzy`
            *   `prefs: PickerPrefs`
        *   **Preview:**
            *   `last_preview_id: u64`, the id of the latest request
            *   `last_req: Option<(u32, u16)>`, the entry index and width of
                the latest request
            *   `was_visible: bool`
            *   `current: Option<Preview>`
            *   `current_is_stale: bool`
            *   `preview_scroll: u16`
            *   `preview_visible: bool`
            *   `preview_width: u16`, the inner width of the preview pane
            *   `preview_rows: u16`
        *   **Other:**
            *   `notice: Option<(String, Instant)>`
            *   `stale_drops: u32`
            *   `ctx: PickerCtx`

        `pub struct PickerCtx` has `pub` fields: `home: Option<PathBuf>`,
        `syn: Arc<SynTheme>`, `ui: UiColors`, `tab_width: usize` and
        `icons: bool`.
    *   **`pub enum FileAction`**: `None`, `Close`, `Open(PathBuf)`,
        `PrefsChanged(PickerPrefs)`.
    *   **`pub fn new(start, open_path, open_dirty, preselect: Option<PathBuf>,
        prefs, ctx, io) -> Self`** takes a `scan_id` from `next_id()` and sends
        the first `ScanRequest`. `preselect` is the file to select when the
        picker reopens after the discard confirmation prompt.
    *   **The selection rule** runs whenever `filtered` is rebuilt. It takes
        the first of these steps that applies:
        1.  `user_moved` is set and the selected entry is still in `filtered`:
            keep it.
        2.  The query is empty and `preselect` is visible: select it.
        3.  The query is empty and the open notebook is visible: select it.
        4.  Otherwise: select the top row, or `None` when `filtered` is empty.

        The Technical Plan's Filtering and selection section states the same
        rule.
    *   **`pub fn on_event(&mut self, ev: FilesEvent) -> bool`** returns
        whether a redraw is needed.
        *   **A `Root`, `Batch` or `Done` with a stale `scan_id`, or a
            `Preview` whose `id` is not `last_preview_id`:** dropped, and
            `stale_drops` goes up by one.
        *   **`Root`:** stores `root`, and the canonical `open_path`.
        *   **`Batch`:** appends to `entries`, and sets `filtered_stale`.
        *   **`Done`:** stores the summary, and clears `scanning`. An empty
            summary from a failed spawn sets `scan_failed`.
        *   **`Preview`:** sets `current`, and clears `current_is_stale`.
    *   **`pub fn handle_key(&mut self, key: KeyEvent) -> FileAction`**
        implements the key table. It changes only state; it never sends a
        preview request.
        *   **Moves** (`↑`, `↓`, `PgUp`, `PgDn` and their aliases):
            *   they clamp at the first and last row, and do not wrap;
            *   `PgUp` and `PgDn` move by `list_rows`;
            *   each move sets `user_moved`.
        *   **Query edits** call `fuzzy.set_query`, set `filtered_stale` and
            clear `user_moved`.
        *   **`Ctrl+F` / `Ctrl+B`** move `preview_scroll` by `preview_rows / 2`.
            It is clamped to `0..=lines.len().saturating_sub(preview_rows)`.
        *   **`Enter`:**
            *   on an empty list, it returns `None`;
            *   on a non-notebook, it sets `notice = ("saturn opens only
                .ipynb files", now)` and returns `None`;
            *   on a notebook, it returns `Open(path)`.
        *   **`Alt+I`:** flips `show_ignored`, sets `filtered_stale`, clears
            `user_moved` and returns `PrefsChanged`.
        *   **`Alt+H` / `Ctrl+H`:**
            1.  Flip `show_hidden`.
            2.  Set the current cancel flag to true, and replace it with a new
                one.
            3.  Take a new `scan_id` from `next_id()`.
            4.  Clear `entries`, `filtered`, `selected`, `summary`, `list_top`,
                `preview_scroll`, `current` and `last_req`.
            5.  Clear `user_moved`. Keep `query`.
            6.  Set `scanning` and `filtered_stale`.
            7.  Send a `ScanRequest`, and return `PrefsChanged`.
    *   **`pub fn push_query(&mut self, s: &str)`** is used for paste. It
        behaves as a query edit.
    *   **`pub fn on_tick(&mut self) -> bool`:**
        *   it clears a notice that is 3 s old or older, and returns true;
        *   it returns true while `scanning`.
    *   **`pub fn is_open_dirty(&self, path: &Path) -> bool`** is
        `self.open_dirty && Some(path) == self.open_path.as_deref()`.
    *   **`impl Drop`** sets the cancel flag, and logs `stale_drops` with
        `debug!`. Dropping `io` drops the `PreviewWorker`, which closes its
        slot.
    *   **`pub fn render(buf: &mut Buffer, area: Rect, p: &mut FilePickerState,
        theme: &Theme)`:**
        1.  **Rebuild `filtered`** when `filtered_stale` is set:
            *   rank the visible entries with `fuzzy.rank`, using
                `display.len()` as the tie-break;
            *   apply the selection rule;
            *   clear the flag.
        2.  **Work out the layout.** Set `preview_visible`, `preview_width`,
            `preview_rows` and `list_rows`.
        3.  **Reset scrolling.** If the selected entry differs from the one in
            `last_req`, set `preview_scroll = 0`.
        4.  **Request a preview.** This is the only place that calls
            `io.preview`. It happens when `preview_visible` is set, an entry is
            selected, and any of these holds:
            *   `!was_visible`;
            *   `current` is `None`;
            *   `last_req` differs from `(selected, preview_width)` in the
                entry;
            *   `last_req` differs in the width, and the selected file is a
                notebook.

            Each request takes its id from `next_id()`, and stores it in
            `last_preview_id`. It sets `last_req`, and sets `current_is_stale`
            when `current` is `Some`.
        5.  **Remember visibility.** Set `was_visible = preview_visible`.
        6.  **Draw** the header, the rows, the preview and the footer, as
            described in the Technical Plan:
            *   The title is ` <shorten_path(clean_name(root), inner_width -
                4)> `, or the starting folder until `Root` arrives.
            *   Matched characters come from `fuzzy.indices`, for ASCII rows on
                screen.
            *   The folder part of a row is cut with `cut_left`.
            *   The preview's bottom border shows the "unsaved edits" note
                first (when `is_open_dirty`), then `note`.
            *   `line_numbers: true` draws a gutter of `digits(total lines) + 1`
                columns. It holds 1-based, right-aligned numbers in
                `ui.line_numbers`, starting from `preview_scroll + 1`.
            *   `current_is_stale` draws the preview with `Modifier::DIM`.
    *   **Tests.** Every test uses `RecordIo`, feeds events by hand, and draws
        into `Buffer::empty(Rect::new(0, 0, w, h))`.
        *   **Layout:**
            *   160×45: the panes are side by side;
            *   100×40: the list sits above the preview;
            *   100×20: only the list shows, and no `IoCall::Preview` is
                recorded;
            *   resizing from 100×20 to 160×45 records one `IoCall::Preview`.
        *   **Selection:**
            *   with a query set and the user moved to entry X, X stays
                selected while later batches add better matches (rule step 1);
            *   with an empty query, an open notebook that arrives in the
                second batch becomes selected (rule step 3);
            *   after a move, it does not;
            *   `preselect` wins over the open notebook (rule step 2).
        *   **Stale events:** a `Batch` with an old `scan_id` is ignored, and
            a `Preview` with an old `id` is not shown.
        *   **`Enter`:**
            *   on `.py`, it gives the notice and `FileAction::None`;
            *   on `.ipynb`, it gives `Open`;
            *   on an empty list, it gives `None`.
        *   **Toggles:**
            *   `Alt+I` shows an ignored entry, and records no new
                `IoCall::Scan`;
            *   `Alt+H` records an `IoCall::Scan` with `show_hidden` set and a
                new `scan_id`, and keeps the query.
        *   **Ticks:** with `Done` already delivered, `on_tick` clears a notice
            dated `Instant::now().checked_sub(Duration::from_secs(4))`. It
            returns true once, then false.
        *   **Drawing:**
            *   a long path keeps its file name whole;
            *   a file name that holds an ESC byte is drawn with `?`;
            *   a non-ASCII row is drawn without match highlights;
            *   the "unsaved edits" note appears only when `open_dirty` is set
                and the selected file is the open notebook.
        *   **`Esc`** gives `Close`.
        *   **Empty state:** with no entries and `Done` received, the screen
            shows `no files under <root> — Esc, then Ctrl+P → New notebook`.

#### 19. `src/config.rs` — commit 7 (Modify)

*   **Type / Scope**: Configuration.
*   **Proposed Changes**:
    *   **`show_hidden`:** new doc comment, "Files picker: list hidden files
        and folders (dot names). Toggle with Alt+H."
    *   **`show_ignored: bool`** (new, default `false`): "Files picker: list
        files matched by .gitignore. Notebooks are always listed. Toggle with
        Alt+I."
    *   **`icons: bool`** (new, default `true`): "Files picker: show Nerd Font
        file-type icons. Needs a Nerd Font, which Ghostty includes."
    *   **Test:** a `config.toml` without the new keys loads with the defaults,
        and `icons = false` parses.

#### 20. `src/app.rs` — commit 8 (Modify)

*   **Type / Scope**: Wiring, and removal of the old browser.
*   **Proposed Changes**:
    *   **Events.** Add `AppEvent::Files(crate::files::FilesEvent)`. Handling:
        when `Overlay::Files(p)` is open, call `p.on_event(ev)` and set
        `dirty_ui` if it returns true. Otherwise drop the event.
    *   **Fields.** Replace `show_hidden: bool` with `picker_prefs:
        PickerPrefs`, set from `cfg.show_hidden` and `cfg.show_ignored`.
    *   **`fn open_file_picker(&mut self, start: PathBuf, preselect:
        Option<PathBuf>)`:**
        *   It builds a `Sink` that sends `AppEvent::Files` on `self.tx`.
        *   It builds `ThreadIo` with `PreviewWorker::spawn(sink.clone())`.
        *   It builds `PickerCtx` from `dirs::home_dir()`,
            `Arc::new(self.theme.syntax.clone())`, `UiColors` from
            `self.theme.ui`, `cfg.tab_width` and `cfg.icons`.
        *   It sets `self.overlay = Some(Overlay::Files(Box::new(
            FilePickerState::new(start, self.nb.path.clone(), self.nb.dirty,
            preselect, self.picker_prefs, ctx, Box::new(io)))))`.
    *   **Call sites:**
        *   `Command::OpenFile` → `open_file_picker(self.cwd(), None)`.
        *   In `App::new`'s startup `match opts.path`:
            *   the `Some(p) if p.is_dir()` arm → `open_file_picker(p, None)`;
            *   the `None` arm → `open_file_picker(current_dir, None)`.
    *   **Overlay keys.** An `Overlay::Files(p)` arm:
        *   **`Close`:** `overlay = None`. Dropping the picker stops its
            workers.
        *   **`Open(path)`:**
            *   Not dirty: `overlay = None`, then `open_notebook(path)`.
            *   Dirty: take `start` from the picker, then show
                `ConfirmKind::OpenDiscard { path, reopen: Some(start) }` with
                the existing "Discard unsaved changes and open …? [y/n]"
                message.
        *   **`PrefsChanged(x)`:** `self.picker_prefs = x`.
    *   **`ConfirmKind::OpenDiscard`** changes from a tuple variant to `{ path:
        PathBuf, reopen: Option<PathBuf> }`.
        *   A new pure helper, `fn reopen_after(kind: &ConfirmKind, yes: bool)
            -> Option<(PathBuf, PathBuf)>`, returns `(start, path)` when the
            picker should reopen.
        *   `confirm(kind, false)` calls the helper. On `Some((start, path))`,
            it calls `open_file_picker(start, Some(path))`.
        *   In the confirm overlay's key handling, the `Esc` / `q` arm also
            calls `confirm(kind, false)` for `OpenDiscard`.
        *   Other kinds keep today's `Esc` behaviour, because `confirm(Quit,
            false)` means "quit without saving".
    *   **Paste.** In the `AppEvent::Terminal(Event::Paste(text))` arm, replace
        the per-overlay `else if` chain with `overlay.paste(text)`. The
        search-input and cell-editor branches stay before it.
    *   **Tick.** In `AppEvent::Tick`, set `dirty_ui` when the open picker's
        `on_tick()` returns true.
    *   **Remove:**
        *   `open_browser`;
        *   the `PaletteAction::Parent`, `PaletteAction::Home` and
            `PaletteAction::ToggleHidden` arms;
        *   the `ItemData::Dir`, `ItemData::File` and `ItemData::NewNotebook`
            arms of `on_palette_accept`;
        *   the `use crate::files::{human_size, shorten_path}` import added in
            commit 5, whose only uses were in `open_browser`.

        After removal, `cargo clippy --all-targets` must report no unused
        items. Code is named by symbol here, not by line number, because
        earlier commits move the lines.
    *   **Test:** unit tests for `reopen_after`:
        *   `OpenDiscard` with `reopen` and `yes = false` gives
            `Some((start, path))`.
        *   With `yes = true`, it gives `None`.
        *   `NewDiscard` gives `None`.

#### 20a. `src/commands.rs` — commit 8 (Modify)

*   **Type / Scope**: Command palette entries.
*   **Proposed Changes**: Rename the `Command::OpenFile` entry from
    `"Open file…"` to `"Find file…"`, to match the help text.

#### 21. `src/main.rs` — commit 8 (Modify)

*   **Type / Scope**: CLI help.
*   **Proposed Changes**: The `PATH` usage line becomes `notebook (.ipynb) to
    open, or a directory to find files in (default: cwd)`.

#### 22. `README.md` — commit 8 (Modify)

*   **Type / Scope**: User documentation.
*   **Proposed Changes**:
    *   **Line 11:** "Fuzzy command palette, file browser and kernel picker"
        becomes "Fuzzy command palette, files picker with preview, and kernel
        picker".
    *   **Line 20:** replace "or `cargo install --path .` to put it in
        `~/.cargo/bin`" with this note: `make install` puts the binary in
        `~/.local/bin`, and `cargo install` puts a second copy in
        `~/.cargo/bin`. When `~/.cargo/bin` comes first on `PATH`, that copy
        hides the other.
    *   **Usage (lines 41–42):** `saturn` and `saturn dir/` "list the files of
        the git repository (or of the directory) in the files picker".
    *   **Keys table:** `Ctrl+O` → "find file (picker with preview)".
    *   **A new "Files picker" paragraph** after the `gt` and `/` paragraphs:
        *   the root rule;
        *   what is listed and what is left out;
        *   that gitignored notebooks are always listed;
        *   the preview rules and limits;
        *   the picker's key table;
        *   that `Alt` keys need Option-as-Meta (`macos-option-as-alt` in
            Ghostty), with `Ctrl+H` and `show_ignored` as fallbacks;
        *   that icons need a Nerd Font, and `icons = false` turns them off.
    *   **Config section:**
        *   `show_hidden`, with its new wording;
        *   `show_ignored` and `icons`;
        *   one sentence that an invalid value makes saturn ignore the whole
            config file and use defaults.
    *   **Themes section:** add one line after its first paragraph. It says
        that highlighting uses the two-face syntax set, which holds `bat`'s
        syntaxes.

### Verification Plan

*   **For every commit:** `cargo build`, `cargo test` and
    `cargo clippy --all-targets`, with no new warnings.
*   **Once, after commit 2:**
    *   `cargo tree -d -i syntect` prints nothing, so there is a single syntect
        copy.
    *   `python_highlighting_uses_semantic_keys` passes.
*   **Unit tests:** `cargo test`.
    *   All existing tests pass, including `tests/roundtrip.rs`, the theme
        tests and the `cell_picker` tests.
    *   `cut_keeps_width` moves from `cell_picker.rs` to `text.rs` unchanged.
    *   The new tests are listed for each file above.
    *   `cargo test --release -- --ignored` runs the timed tests and the
        optional `git check-ignore` comparison.
*   **Formatting:** run `rustfmt --edition 2024` on the *new* files only. The
    repository is not rustfmt-clean, so never run `cargo fmt`.
*   **Removal check:** `rg 'PaletteKind::Files|open_browser|edtui::SYNTAX_SET'
    src` returns nothing.
*   **Coverage:** every row of the classification table, every rule of the
    ignore stack, and every `FileAction` has at least one test. The stack rules
    are: inherited parent, deepest `.gitignore` first, `!` stops the search,
    `info/exclude`, global, outside a repository, and nested repository. There
    is no coverage tool and no percentage target.
*   **Manual checks in tmux.** Build with `cargo build`. Start each session
    like this:

    ```bash
    tmux new-session -d -s sat -x 160 -y 45 "SATURN_LOG=debug target/debug/saturn <dir> --log /tmp/saturn-test.log; sleep 60"
    ```

    *   Wait about 3.5 s before the first key. saturn's terminal-capability
        queries time out inside tmux.
    *   Drive it with `tmux send-keys -t sat …`, and read the screen with
        `tmux capture-pane -p -t sat`.
    *   Send `Escape` in its own `send-keys` call, then run `sleep 0.2`.
        Otherwise tmux joins it to the next key and saturn reads `Alt+<key>`.

    1.  **Counter.** On a repository with about 1,000–2,000 files after
        `.gitignore`, the counter's total equals this number: the paths from
        `git ls-files -co --exclude-standard` that are not under a dot-folder,
        are not dot-named and are not under a skip-list folder, plus the
        gitignored `.ipynb` files that are not under a dot-folder.
    2.  **Timing.** The scan's `info!` line shows under 100 ms. The preview
        `debug!` lines show under 50 ms each after the first. No "slow frame"
        line appears.
    3.  **`saturn` started in `~`.**
        *   The picker's first frame appears.
        *   `Esc` closes it during the scan.
        *   The counter stops changing, and shows `+` if a cap was hit.
        *   No `Library/` path appears.
    4.  **A repository with `.venv/`.** No `site-packages` path appears, even
        after `Alt+H` and `Alt+I`.
    5.  **Holding `↓`.** Send 100 `Down` keys in one `send-keys` call. The
        selection ends 100 rows down. The preview shows that file within 50 ms
        of the last key, and the log has a preview `debug!` line for it.
    6.  **Special files**, each with its expected result:

        | File | Expected |
        |---|---|
        | `mkfifo p` | `[fifo]` |
        | a 600 KiB minified `.js` | `too large to preview (600.0K)` |
        | a PNG | `binary · …` |
        | a CRLF text file | no `^M` in `capture-pane` |
        | a file with ANSI colour codes | plain text, no colours in `capture-pane -e` |
        | a symlink to a folder | `[folder]` |

    7.  **Discard confirm.** With the open notebook edited and unsaved:
        *   `Enter` on another notebook, then `n`, reopens the picker with
            that notebook selected and the query empty.
        *   `Enter` on a notebook that was deleted from another pane, then
            `y`, shows the load error. After that, `q` still asks to save.
    8.  **Resize.** In a 160×45 session the panes are side by side.
        `tmux resize-window -t sat -x 100 -y 40` shows the list above the
        preview. `-x 100 -y 20` shows the list only.
    9.  **A deleted file.** Delete the selected file from another pane, then
        select it again. The preview shows `cannot read: …`.
    10. **Cells after the two-face swap.** Open `assets/sample.ipynb` (Python
        code and markdown cells). Compare `capture-pane -e` output from before
        commit 2 and after it. Write any visible difference into the change
        description.
*   **In Ghostty** (by the user; tmux cannot show these):
    *   Nerd Font icons draw inside their 2-column slot.
    *   `Alt+H` and `Alt+I` reach saturn.
    *   Inline images from the notebook underneath do not show through the
        picker.
*   **Install:** `make install`, which installs to `~/.local/bin/saturn`. Do not
    use `cargo install`. It puts a second copy in `~/.cargo/bin`, and that copy
    shadows `~/.local/bin/saturn` when `~/.cargo/bin` comes first on `PATH`.
