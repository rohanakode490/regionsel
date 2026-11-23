# regionsel

Copy clean text out of full-screen terminal apps, from tmux.

Terminal selection is linear or rectangular, so in a TUI (Claude Code, lazygit, k9s, vim splits,
...) a drag also grabs borders, neighbouring panels, line numbers and hard line breaks.
regionsel captures the pane, infers the regions on screen (blocks, panels, split panes,
paragraphs), and lets you grow a selection **semantically**, word > line > paragraph/block >
panel > pane, then copies the text without the decoration.

It works from the screen's cells only (characters, colours, attributes), so it is not tied to
any one app.

```
┌ Files ──────────┐╭ Diff ──────────────────────────╮     Space on "src/lib.rs":  src/lib.rs
│ M src/lib.rs    ││ -    println!("hello");         │     Space again:           src/lib.rs
│ M src/detect.rs ││ +    println!("hello, world");  │                            src/detect.rs
└─────────────────┘╰─────────────────────────────────╯     (no borders, no diff column)
```

## Requirements
- tmux **3.3 or newer** (`display-popup -B`). Ubuntu 24.04 ships 3.4; 22.04 ships 3.2a, which does not work.
- Rust 1.75+ to build.
- Clipboard: `wl-copy`, `xclip`, `xsel`, `pbcopy` or `clip.exe` if present. The tmux paste buffer is
  always set too (and tmux forwards it to your terminal via OSC 52, which also works over SSH).

## Install
    cargo build --release
    cp target/release/regionsel ~/.local/bin/
    regionsel init >> ~/.tmux.conf     # prints a binding (prefix + y) and a Wayland env fix
    tmux source ~/.tmux.conf

Then press `prefix + y`. With the usual `prefix + [` muscle memory in mind, the keys inside
the popup are vim-like: move with `h j k l`, press `Space` to grow the selection, `Enter` to copy.

## Keys (in the popup)
    h j k l / arrows   move          w b  word       0 $  line start/end     gg G  top/bottom
    5j 3w              counts
    Space / Backspace  expand / shrink semantic selection (word > line > block > panel > pane)
    v  V  Ctrl-v       manual char / line / block selection (literal copy)
    Enter              copy (no selection yet = copy the cursor's line)
    Ctrl-d Ctrl-u      half page down / up       Ctrl-f Ctrl-b  PgDn PgUp   full page
    Esc  q  Ctrl-c     cancel                    (all of these can be rebound, see Configuration)

## Configuration
Key bindings and the scrollback size are configurable. Print the defaults, save them, edit:

    mkdir -p ~/.config/regionsel && regionsel config > ~/.config/regionsel/config

Your file is applied on top of the built-in defaults (`$XDG_CONFIG_HOME/regionsel/config`, or
`$REGIONSEL_CONFIG`). The format is a few lines, documented in the file itself:

    unbind q                  # stop q from cancelling
    bind x cancel
    bind ctrl-j half-page-down
    unbind g g                # free "g" ...
    bind g top                # ... to use it on its own
    scrollback 5000

A typo never disables copying: the bad line is skipped and tmux shows "config line N: ...".
Counts (`5j`) are fixed; the digits 1-9 cannot be rebound.

## Scrollback and mouse
The popup captures up to 2000 lines of history above the visible screen (`popup <pane>
--scrollback N`, 0 = visible screen only). It opens showing exactly what the pane showed; scroll
up with the keys above or the mouse wheel. The top rung of the ladder ("Pane") is everything that
was captured, history included. The screen can be smaller than the pane (tmux shrinks popups that
don't fit); the view then scrolls with the cursor, in both directions.

Mouse: `regionsel init` prints an optional `Alt+click` binding (needs `set -g mouse on`). Alt+click
opens the picker with the clicked word selected. Inside the popup, click inside the highlighted
selection to grow it one level, or click elsewhere to start over there. In v / V / Ctrl-v modes a
click moves the cursor.

## How it works
`prefix + y` runs `regionsel popup`, which captures the pane *first* (`capture-pane -p -e -N`),
writes the screen to a private (mode 0600) snapshot file, then opens a popup on that snapshot. What
you select is what you saw when you pressed the key, even if the app keeps repainting underneath.

The code is split so the interesting part has no I/O:

| | |
|---|---|
| `src/lib.rs`, `detect.rs`, `keymap.rs`, `view.rs` | **Pure core.** Grid in, region tree out, clean text out; key sequences and viewport arithmetic. No terminal, tmux or clipboard. |
| `src/ansi.rs` | `capture-pane` output to a grid, via the `vt100` crate. |
| `src/tmux.rs`, `clipboard.rs`, `snapshot.rs`, `settings.rs`, `ui.rs`, `main.rs` | Thin frontend. |

Detectors each look for one kind of evidence and never talk to each other: **Border** (closed
box-drawing rectangles, titled borders included), **BackgroundRun** (uniform background colour or
reverse video), **Separator** (vim-style reverse-video split dividers) and a second-phase
**IndentPrefix** refiner (blank-separated paragraphs, indented bodies). A containment tree reconciles
them; extraction then removes borders and padding and rejoins soft-wrapped lines.

## What has been tested, and what has not
Tested: the pure core (unit tests), eleven recorded real screens (Claude Code, lazygit, k9s, btop,
htop, vim splits, a shell) plus one hand-built one, as snapshot fixtures, and the full popup path in a real tmux 3.4 with a
pty client: snapshot-before-popup, scrollback, Alt+click, config, viewport scrolling
(`scripts/e2e-popup.py`).

Not tested: real desktop clipboards (`wl-copy`/`xclip` were not exercised, only the tmux buffer),
macOS beyond the unit tests, tmux older than 3.4, a narrow Claude Code pane with many wrapped
paragraphs, Claude Code diff views.

Known limits: panels hidden behind a popup/modal are not detected (their borders are occluded);
btop's nested layout is only partly recognised; an nvim divider drawn without reverse video is
not detected (untested); the line-number gutter is copied as text; horizontal splits are not detected.
Detection is heuristic. When it guesses wrong, `v` / `V` / `Ctrl-v` give you plain literal selection.

## Development
    cargo test                                        # unit tests + fixture snapshots
    cargo clippy --all-targets -- -D warnings && cargo fmt --check
    cargo build --release && python3 scripts/e2e-popup.py   # real tmux; needs tmux >= 3.3

    regionsel dump --pane %1 --row 10 --col 5         # print the expansion ladder at a position
    regionsel dump --file tests/fixtures/<app>/<case>.ansi --size 120x40 --row 10 --col 5

See [CONTRIBUTING.md](CONTRIBUTING.md) for recording fixtures from a new app.
