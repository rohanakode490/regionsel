mod clipboard;
mod tmux;
mod ui;

use std::env;
use std::fs;

use regionsel::{ansi, Engine, Grid, Pos};

const USAGE: &str = "\
regionsel: region-aware copy for terminal screens

  regionsel init                      print the tmux.conf snippet
  regionsel popup <pane>              open the picker over a pane (used by the key binding)
  regionsel pick --pane <pane>        the picker itself (runs inside the popup)
  regionsel dump --pane <pane> [--row N --col N]
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
         bind-key y run-shell -b \"'{}' popup '#{{pane_id}}'\"\n\
         # Wayland clipboard: let popups see WAYLAND_DISPLAY\n\
         set -ga update-environment ' WAYLAND_DISPLAY'",
        exe.display()
    );
    Ok(())
}

fn cmd_popup(args: &[String]) -> Result<(), String> {
    let pane = args.first().ok_or("usage: regionsel popup <pane-id>")?;
    let info = tmux::pane_info(pane)?;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    tmux::popup(&info, &exe.to_string_lossy())
}

fn cmd_pick(args: &[String]) -> Result<(), String> {
    let pane = flag(args, "--pane").ok_or("usage: regionsel pick --pane <pane-id>")?;
    let info = tmux::pane_info(pane)?;
    // Refinement for later: capture in `popup` BEFORE the popup opens and pass a snapshot file,
    // so what you select is exactly what you saw when you pressed the key.
    let grid = ansi::grid_from_capture(&tmux::capture(pane)?, info.width, info.height);
    let engine = Engine::with_defaults();
    let analysis = engine.analyze(&grid);

    let start = Pos {
        row: info.cursor_y.min(info.height.saturating_sub(1)),
        col: info.cursor_x.min(info.width.saturating_sub(1)),
    };
    let picked = ui::run(&analysis, start).map_err(|e| format!("terminal error: {e}"))?;

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
        let info = tmux::pane_info(pane)?;
        ansi::grid_from_capture(&tmux::capture(pane)?, info.width, info.height)
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
