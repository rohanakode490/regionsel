mod clipboard;
mod snapshot;
mod tmux;
mod ui;

use std::env;
use std::fs;

use regionsel::{ansi, Engine, Grid, Pos};

const USAGE: &str = "\
regionsel: region-aware copy for terminal screens

  regionsel init                      print the tmux.conf snippet
  regionsel popup <pane> [--scrollback N] [--click X,Y]
                                      open the picker over a pane (used by the key binding).
                                      --scrollback: history lines to include (default 2000, 0 = none)
                                      --click: pane-relative mouse position; opens with that word selected
  regionsel pick --pane <pane>        the picker itself (runs inside the popup; captures live)
  regionsel pick --snapshot F --size WxH --cursor X,Y [--click]
                                      the picker on a snapshot taken before the popup opened
  regionsel dump --pane <pane> [--scrollback N] [--row N --col N]
  regionsel dump --file F.ansi --size WxH [--row N --col N]
                                      print the expansion ladder; for developing detectors
                                      and recording fixtures
";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("init") => cmd_init(),
        Some("popup") => cmd_popup(&args[1..]),
        Some("pick") => cmd_pick(&args[1..]),
        Some("dump") => cmd_dump(&args[1..]),
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    };
    if let Err(e) = result {
        // Inside a popup, stderr vanishes with it, so also surface errors in tmux.
        if matches!(args.first().map(String::as_str), Some("pick") | Some("popup")) {
            tmux::message(&format!("regionsel: {e}"));
        }
        eprintln!("regionsel: {e}");
        std::process::exit(1);
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn cmd_init() -> Result<(), String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    println!(
        "# ~/.tmux.conf\n\
         bind-key y run-shell -b \"'{exe}' popup '#{{pane_id}}'\"\n\
         # Wayland clipboard: let popups see WAYLAND_DISPLAY\n\
         set -ga update-environment ' WAYLAND_DISPLAY'\n\
         # Optional: Alt+click opens the picker with the clicked word selected. Needs\n\
         # `set -g mouse on` (not set here: it changes how tmux handles every click).\n\
         bind-key -n M-MouseDown1Pane run-shell -b \"'{exe}' popup '#{{pane_id}}' --click '#{{mouse_x}},#{{mouse_y}}'\"",
        exe = exe.display()
    );
    Ok(())
}

/// Default number of scrollback lines included in a capture.
const DEFAULT_SCROLLBACK: u32 = 2000;

/// A capture plus how it maps onto the pane: `hist` rows of scrollback above `info.height`.
struct Captured {
    raw: String,
    info: tmux::PaneInfo,
    rows: u16,
    hist: u16,
}

fn capture_pane(pane: &str, args: &[String], default_limit: u32) -> Result<Captured, String> {
    let limit = flag(args, "--scrollback").and_then(|v| v.parse::<u32>().ok()).unwrap_or(default_limit);
    let info = tmux::pane_info(pane)?;
    let raw = tmux::capture_with_history(pane, info.history.min(limit))?;
    // Trust the capture, not the arithmetic: derive the real row count from it.
    let rows = (snapshot::count_rows(&raw).max(info.height as usize)).min(u16::MAX as usize) as u16;
    let hist = rows - info.height;
    Ok(Captured { raw, info, rows, hist })
}

fn cmd_popup(args: &[String]) -> Result<(), String> {
    let pane = args.first().ok_or("usage: regionsel popup <pane-id> [--scrollback N] [--click X,Y]")?;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    // Capture NOW, before the popup exists: what you select is what you saw at the keypress.
    let cap = capture_pane(pane, args, DEFAULT_SCROLLBACK)?;
    let snap = snapshot::write(&cap.raw).map_err(|e| format!("cannot write snapshot: {e}"))?;
    let click = flag(args, "--click").and_then(snapshot::parse_cursor);
    // Pane coordinates -> grid coordinates: the scrollback sits above the visible screen.
    let (x, y) = click.unwrap_or((cap.info.cursor_x, cap.info.cursor_y));
    let pick = tmux::PickArgs {
        cols: cap.info.width,
        rows: cap.rows,
        cursor: (x.min(cap.info.width.saturating_sub(1)), (y + cap.hist).min(cap.rows.saturating_sub(1))),
        click: click.is_some(),
    };
    let result = tmux::popup(&cap.info, &exe.to_string_lossy(), &snap, &pick);
    let _ = fs::remove_file(&snap); // pick deletes it too; this covers a popup that never ran
    result
}

fn cmd_pick(args: &[String]) -> Result<(), String> {
    let (grid, start) = if let Some(file) = flag(args, "--snapshot") {
        let (w, h) = flag(args, "--size").and_then(snapshot::parse_size).ok_or("--snapshot needs --size WxH")?;
        let (cx, cy) = flag(args, "--cursor").and_then(snapshot::parse_cursor).unwrap_or((0, 0));
        let raw = snapshot::take(std::path::Path::new(file)).map_err(|e| format!("{file}: {e}"))?;
        (ansi::grid_from_capture(&raw, w, h), Pos { row: cy.min(h.saturating_sub(1)), col: cx.min(w.saturating_sub(1)) })
    } else {
        let pane = flag(args, "--pane").ok_or("usage: regionsel pick --pane <pane-id> | --snapshot F --size WxH")?;
        let cap = capture_pane(pane, args, DEFAULT_SCROLLBACK)?;
        let (w, h) = (cap.info.width, cap.rows);
        let start = Pos {
            row: (cap.info.cursor_y + cap.hist).min(h.saturating_sub(1)),
            col: cap.info.cursor_x.min(w.saturating_sub(1)),
        };
        (ansi::grid_from_capture(&cap.raw, w, h), start)
    };
    let engine = Engine::with_defaults();
    let analysis = engine.analyze(&grid);

    let picked = ui::run(&analysis, start, args.iter().any(|a| a == "--click"))
        .map_err(|e| format!("terminal error: {e}"))?;

    if let Some(text) = picked {
        if text.is_empty() {
            tmux::message("regionsel: nothing selected");
        } else {
            let backends = clipboard::copy(&text);
            if backends.is_empty() {
                return Err("no clipboard backend worked".into());
            }
            tmux::message(&format!(
                "regionsel: copied {} chars ({})",
                text.chars().count(),
                backends.join(", ")
            ));
        }
    }
    Ok(())
}

fn cmd_dump(args: &[String]) -> Result<(), String> {
    let grid: Grid = if let Some(pane) = flag(args, "--pane") {
        // Row/col are visible-screen coordinates unless --scrollback is given.
        let cap = capture_pane(pane, args, 0)?;
        ansi::grid_from_capture(&cap.raw, cap.info.width, cap.rows)
    } else if let Some(file) = flag(args, "--file") {
        let (w, h) = flag(args, "--size")
            .and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
            .ok_or("--file needs --size WxH")?;
        let raw = fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        ansi::grid_from_capture(&raw, w, h)
    } else {
        return Err("dump needs --pane or --file".into());
    };

    let num = |name: &str| flag(args, name).and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
    let pos = Pos { row: num("--row"), col: num("--col") };
    let analysis = Engine::with_defaults().analyze(&grid);

    for (i, t) in analysis.ladder_at(pos).iter().enumerate() {
        println!(
            "[{i}] {:?}  rows {}..{}  cols {}..{}",
            t.kind, t.rect.row0, t.rect.row1, t.rect.col0, t.rect.col1
        );
        for line in analysis.extract(t).lines() {
            println!("      | {line}");
        }
    }
    Ok(())
}
