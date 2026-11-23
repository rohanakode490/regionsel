# regionsel

Region-aware copy for terminal screens. tmux popup frontend + a pure-logic core
(`src/lib.rs`: grid in, region tree out, clean text out).

## Build and install
    cargo build --release
    cp target/release/regionsel ~/.local/bin/
    regionsel init >> ~/.tmux.conf     # prints a binding (prefix + y) and a Wayland env fix
    tmux source ~/.tmux.conf

Requires tmux >= 3.3 (`display-popup -B`). Check with `tmux -V`.

## Keys (in the popup)
    h j k l / arrows   move          w b  word       0 $  line start/end     gg G  top/bottom
    5j 3w              counts
    Space / Backspace  expand / shrink semantic selection (word > line > region > pane)
    v  V  Ctrl-v       manual char / line / block selection (literal copy)
    Enter              copy (no selection yet = copy the cursor's line)
    Ctrl-d Ctrl-u      half page down / up       Ctrl-f Ctrl-b  PgDn PgUp   full page
    Esc  q  Ctrl-c     cancel

## Scrollback and mouse
The popup captures up to 2000 lines of history above the visible screen (`popup <pane>
--scrollback N`, 0 = visible screen only). It opens showing exactly what the pane showed; scroll
up with the keys above or the mouse wheel. The top rung of the ladder ("Pane") is everything that
was captured, history included. The screen can be smaller than the pane (tmux shrinks popups that
don't fit); the view then scrolls with the cursor, in both directions.

Mouse: `regionsel init` prints an optional `Alt+click` binding (needs `set -g mouse on`). Alt+click
opens the picker with the clicked word selected. Inside the popup, click inside the highlighted
selection to grow it one level (word > line > block > pane), or click elsewhere to start over
there. In v / V / Ctrl-v modes a click moves the cursor.

## How a copy works
`prefix + y` runs `regionsel popup`, which captures the pane *first*, writes the screen to a
private (0600) snapshot file, then opens the popup on that snapshot. What you select is what
you saw when you pressed the key, even if the app keeps repainting underneath.
`scripts/e2e-popup.py` checks this in a real tmux.

## Developing detectors
    regionsel dump --pane %1 --row 10 --col 5
    scripts/record-fixture.sh <app> <case> [pane]    # -> tests/fixtures/<app>/<case>.{ansi,expected.json}
    regionsel dump --file tests/fixtures/<app>/<case>.ansi --size 120x40 --row 10 --col 5
    cargo test --test fixtures                       # compare every fixture with its snapshot
    UPDATE_FIXTURES=1 cargo test --test fixtures     # rewrite snapshots; READ the diff before committing

## Status
Detectors in `src/detect.rs`: Border (titled boxes too), BackgroundRun, Separator (reverse-video
vim splits) and the IndentPrefix refiner (paragraphs, indented bodies).
