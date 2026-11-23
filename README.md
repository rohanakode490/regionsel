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
    Esc  q  Ctrl-c     cancel

## Developing detectors
    regionsel dump --pane %1 --row 10 --col 5
    scripts/record-fixture.sh <app> <case> [pane]    # -> tests/fixtures/<app>/<case>.{ansi,expected.json}
    regionsel dump --file tests/fixtures/<app>/<case>.ansi --size 120x40 --row 10 --col 5
    cargo test --test fixtures                       # compare every fixture with its snapshot
    UPDATE_FIXTURES=1 cargo test --test fixtures     # rewrite snapshots; READ the diff before committing

## Status
BorderDetector and BackgroundRunDetector are implemented in `src/detect.rs`.
