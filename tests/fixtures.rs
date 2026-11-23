//! Snapshot tests over recorded screens.
//!
//!   tests/fixtures/<app>/<case>.ansi            output of `tmux capture-pane -p -e -N`
//!   tests/fixtures/<app>/<case>.expected.json   size + probes (position -> expected ladder)
//!
//! expected.json:
//!   { "size": [cols, rows],
//!     "note": "free text, ignored",
//!     "probes": [ { "pos": [row, col],
//!                   "ladder": [ { "kind": "Word", "text": "..." }, ... ] } ] }
//!
//! Run `UPDATE_FIXTURES=1 cargo test --test fixtures` to rewrite the `ladder` of every probe
//! from the current output. A regenerated snapshot only guards against regressions; it is
//! NOT evidence that the output is right. Read the diff before committing it.

use std::fs;
use std::path::{Path, PathBuf};

use regionsel::{ansi, Engine, Pos};
use serde_json::{json, Value};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn find_ansi(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            find_ansi(&p, out);
        } else if p.extension().is_some_and(|e| e == "ansi") {
            out.push(p);
        }
    }
}

fn label(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).with_extension("").display().to_string()
}

/// Actual ladder for one probe, in the same shape as the expected JSON.
fn actual_ladder(analysis: &regionsel::Analysis, row: u16, col: u16) -> Value {
    let steps: Vec<Value> = analysis
        .ladder_at(Pos { row, col })
        .iter()
        .map(|t| json!({ "kind": format!("{:?}", t.kind), "text": analysis.extract(t) }))
        .collect();
    Value::Array(steps)
}

#[test]
fn fixtures_match_snapshots() {
    let root = fixtures_root();
    let update = std::env::var_os("UPDATE_FIXTURES").is_some();
    let mut files = Vec::new();
    find_ansi(&root, &mut files);

    let mut failures: Vec<String> = Vec::new();
    for ansi_path in &files {
        let name = label(&root, ansi_path);
        let json_path = ansi_path.with_extension("expected.json");

        let raw = fs::read_to_string(ansi_path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut expected: Value = match fs::read_to_string(&json_path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| panic!("{name}: bad json: {e}")),
            Err(_) => {
                failures.push(format!("{name}: missing {}", json_path.display()));
                continue;
            }
        };

        let size = &expected["size"];
        let (Some(cols), Some(rows)) = (size[0].as_u64(), size[1].as_u64()) else {
            failures.push(format!("{name}: expected.json needs \"size\": [cols, rows]"));
            continue;
        };
        let grid = ansi::grid_from_capture(&raw, cols as u16, rows as u16);
        let analysis = Engine::with_defaults().analyze(&grid);

        let Some(probes) = expected["probes"].as_array_mut() else {
            failures.push(format!("{name}: expected.json needs a \"probes\" array"));
            continue;
        };
        if probes.is_empty() {
            failures.push(format!("{name}: no probes, nothing is being tested"));
            continue;
        }

        let mut changed = false;
        for probe in probes.iter_mut() {
            let (Some(row), Some(col)) = (probe["pos"][0].as_u64(), probe["pos"][1].as_u64()) else {
                failures.push(format!("{name}: probe without \"pos\": [row, col]"));
                continue;
            };
            let got = actual_ladder(&analysis, row as u16, col as u16);
            if probe["ladder"] != got {
                if update {
                    probe["ladder"] = got;
                    changed = true;
                } else {
                    failures.push(format!(
                        "{name} @ ({row},{col})\n--- expected\n{}\n--- actual\n{}",
                        serde_json::to_string_pretty(&probe["ladder"]).unwrap(),
                        serde_json::to_string_pretty(&got).unwrap()
                    ));
                }
            }
        }
        if update && changed {
            let mut s = serde_json::to_string_pretty(&expected).unwrap();
            s.push('\n');
            fs::write(&json_path, s).unwrap();
            eprintln!("updated {name}");
        }
    }

    assert!(failures.is_empty(), "{} fixture failure(s):\n\n{}", failures.len(), failures.join("\n\n"));
}
