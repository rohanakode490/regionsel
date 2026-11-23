# Changelog

## Unreleased (0.1.0)
First public version.

- Region inference from the screen's cells: titled box borders, background colour / reverse-video
  blocks, vim-style split dividers, blank-separated paragraphs and indented bodies.
- Semantic expansion (word > line > paragraph/block > panel > pane), manual v / V / Ctrl-v modes.
- Clean extraction: borders and padding removed, soft-wrapped lines rejoined (only where a word
  wrapper would have broken the line), code never unwrapped.
- tmux popup frontend: snapshot taken before the popup opens, scrollback (default 2000 lines),
  scrolling viewport, mouse (Alt+click to open, click to grow), configurable key bindings.
- Clipboard: wl-copy / xclip / xsel / pbcopy / clip.exe plus the tmux buffer and OSC 52.
