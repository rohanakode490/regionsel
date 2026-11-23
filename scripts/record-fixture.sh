#!/usr/bin/env bash
# Record a live tmux pane as a fixture.
#
#   scripts/record-fixture.sh <app> <case> [pane]
#
# Run it from a different pane/window than the one you record (or pass the pane id), with the
# TUI showing exactly the screen you want. Creates:
#   tests/fixtures/<app>/<case>.ansi
#   tests/fixtures/<app>/<case>.expected.json   (size + one probe at the cursor)
# Then add probes (positions you care about; find them with `regionsel dump --file ...`) and
# run:  UPDATE_FIXTURES=1 cargo test --test fixtures   and READ the resulting JSON.
set -euo pipefail

app=${1:?usage: record-fixture.sh <app> <case> [pane]}
case_=${2:?usage: record-fixture.sh <app> <case> [pane]}
pane=${3:-}

[[ $app =~ ^[A-Za-z0-9_-]+$ && $case_ =~ ^[A-Za-z0-9_-]+$ ]] || { echo "app/case: [A-Za-z0-9_-] only" >&2; exit 2; }
command -v tmux >/dev/null || { echo "tmux not found" >&2; exit 1; }
target=()
[[ -n $pane ]] && target=(-t "$pane")

dir="$(cd "$(dirname "$0")/.." && pwd)/tests/fixtures/$app"
mkdir -p "$dir"
ansi="$dir/$case_.ansi"
json="$dir/$case_.expected.json"
[[ -e $ansi || -e $json ]] && { echo "refusing to overwrite $dir/$case_.*" >&2; exit 1; }

# -N keeps trailing spaces so background colour runs and full-width rows survive.
tmux capture-pane -p -e -N "${target[@]}" > "$ansi"
read -r w h cy cx < <(tmux display-message -p "${target[@]}" '#{pane_width} #{pane_height} #{cursor_y} #{cursor_x}')

cat > "$json" <<JSON
{
  "size": [$w, $h],
  "note": "recorded $(date -u +%F) from tmux $(tmux -V | cut -d' ' -f2); app: $app",
  "probes": [
    { "pos": [$cy, $cx], "ladder": [] }
  ]
}
JSON
echo "recorded ${w}x${h} -> $ansi"
echo "next: add probes to $json, then UPDATE_FIXTURES=1 cargo test --test fixtures"
echo "      (check that no secrets / private paths are in $ansi before committing it)"
