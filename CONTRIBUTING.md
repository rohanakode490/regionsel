# Contributing

## Ground rules
- New detection or extraction logic goes in the **pure core** (`src/lib.rs`, `src/detect.rs`)
  with unit tests. Frontends (`ui.rs`, `tmux.rs`, ...) stay thin.
- Detectors look for one kind of evidence each and don't depend on each other.
- Prefer evidence over assumption: look at the raw escapes of a real capture to see what an
  app actually emits before writing a heuristic. If a heuristic is fragile, say so in a comment
  and add a fixture that would prove it.
- Keep dependencies minimal and say why in the PR when you add one.
- Must pass before a PR: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`. The minimum Rust version is 1.75 (CI checks it); don't use newer std APIs.

## Fixtures: how support for an app is proven
A fixture is a recorded screen plus a snapshot of what the tool does with it:

    tests/fixtures/<app>/<case>.ansi            output of `tmux capture-pane -p -e -N`
    tests/fixtures/<app>/<case>.expected.json   size, probe positions, and the expected ladder

1. In tmux, put the app in the state you want (a throwaway repo / dummy data, please).
2. From another pane: `scripts/record-fixture.sh <app> <case> <pane-id>`.
3. **Scrub it.** Fixtures are public. Check for tokens, emails, usernames, private repo names,
   home paths: `grep -a -n -i -E 'token|secret|password|@|/home/' tests/fixtures/<app>/<case>.ansi`.
   If you edit the file, replace text with the same number of characters; columns matter.
4. Add probe positions (`"pos": [row, col]`) to the `.expected.json`, using
   `regionsel dump --file ... --row N --col N` to find interesting cells.
5. `UPDATE_FIXTURES=1 cargo test --test fixtures` fills in the expected ladders.
   **A regenerated snapshot is only a regression guard, not proof of correct output.** Read
   the diff. If the output is wrong, say so in a `"known_issue"` field instead of approving it.

Changing a detector? Run the fixtures before and after and look at which snapshots changed.
Every change should be explainable.

## Testing the popup
`scripts/e2e-popup.py` drives a real tmux (needs >= 3.3) on its own server (`-L` socket, no user
config), so it can't touch your sessions. It is timing-based; rerun once before assuming a failure.
