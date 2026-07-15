//! Headless driver for the PCBForge console (see AGENT_DEBUGGING.md).
//!
//! Steps the real `ConsoleApp` with no window via `egui_kittest`, drives its
//! widgets through the accessibility tree, dumps widget/app state, and (with a
//! GPU adapter) renders frames to PNG. Reads a script from stdin or a file arg;
//! each line prints `OK ...` or `ERR ...`, and the process exits non-zero if any
//! command failed. Lines starting with `#` and blank lines are ignored.
//!
//!   printf 'tree\nclick "⟳ Refresh"\nstate\n' | cargo run -p ui --example debug_driver
//!   cargo run -p ui --example debug_driver -- script.txt

use std::io::Read;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use ui::ConsoleApp;

fn main() {
    // Read the script from a file arg or stdin.
    let script = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(2);
        }),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).expect("read stdin");
            s
        }
    };

    // A fresh app each run (deterministic replay). Temp DB, `true` as the verb
    // command so shelling a CLI verb is a harmless no-op.
    let db = std::env::temp_dir().join("pcbforge-debug-driver.sqlite");
    let app = ConsoleApp::new(db, vec!["true".to_string()]);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_state(|ctx, app: &mut ConsoleApp| app.ui(ctx), app);
    harness.run();

    let mut failed = false;
    for raw in script.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match run_command(&mut harness, line) {
            Ok(msg) => println!("OK {msg}"),
            Err(msg) => {
                println!("ERR {msg}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn run_command(harness: &mut Harness<'_, ConsoleApp>, line: &str) -> Result<String, String> {
    let args = tokenize(line);
    let cmd = args[0].as_str();
    match cmd {
        "tree" => {
            let mut out = String::new();
            let root = harness.kittest_state().root();
            dump_tree(&root, 0, &mut out);
            Ok(format!("tree\n{}", out.trim_end()))
        }
        "state" => Ok(format!("state\n{}", harness.state().debug_summary())),
        "click" => {
            let label = arg(&args, 1, "click <label>")?;
            let node = find(harness, &label)?;
            node.click();
            harness.run();
            Ok(format!("click {label:?}"))
        }
        "type" => {
            let label = arg(&args, 1, "type <label> <text>")?;
            let text = args.get(2..).map(|r| r.join(" ")).unwrap_or_default();
            let node = find(harness, &label)?;
            node.focus();
            node.type_text(&text);
            harness.run();
            Ok(format!("type {text:?} into {label:?}"))
        }
        "set" => {
            let label = arg(&args, 1, "set <label> <value>")?;
            let value = arg(&args, 2, "set <label> <value>")?;
            let node = find(harness, &label)?;
            // kittest 0.30 has no accesskit SetValue, so drive it as text entry:
            // focus, type the value, commit. Best on editable numeric fields.
            node.focus();
            node.type_text(&value);
            node.key_press(egui_kittest::kittest::Key::Enter);
            harness.run();
            Ok(format!("set {label:?} = {value}"))
        }
        "key" => {
            let name = arg(&args, 1, "key <name>")?;
            let key = parse_key(&name).ok_or_else(|| format!("unknown key {name:?}"))?;
            harness.press_key(key);
            harness.run();
            Ok(format!("key {name}"))
        }
        "step" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
            for _ in 0..n {
                harness.step();
            }
            Ok(format!("step {n}"))
        }
        "settle" => {
            harness.run();
            Ok("settle".into())
        }
        "screenshot" => {
            let path = arg(&args, 1, "screenshot <path>")?;
            screenshot(harness, &path)
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

/// Find a node by exact label, then by substring, taking the first match if
/// several share a label (the `query_by_*` singular forms panic on >1 match).
fn find<'a>(
    harness: &'a Harness<'_, ConsoleApp>,
    label: &'a str,
) -> Result<egui_kittest::kittest::Node<'a>, String> {
    if let Some(n) = harness.query_all_by_label(label).next() {
        return Ok(n);
    }
    harness
        .query_all_by_label_contains(label)
        .next()
        .ok_or_else(|| format!("no widget matching label {label:?}"))
}

/// Render the current frame to a PNG. Needs a wgpu adapter; without one this
/// returns an error and the run continues.
fn screenshot(harness: &Harness<'_, ConsoleApp>, path: &str) -> Result<String, String> {
    let path = path.to_string();
    let harness_ref = harness;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let image = egui_kittest::wgpu::TestRenderer::new().render(harness_ref);
        image
            .save(&path)
            .map(|_| (image.width(), image.height()))
            .map_err(|e| e.to_string())
    }));
    match result {
        Ok(Ok((w, h))) => Ok(format!("screenshot {path} ({w}x{h})")),
        Ok(Err(e)) => Err(format!("screenshot {path}: {e}")),
        Err(_) => Err(format!(
            "screenshot {path}: no GPU adapter (source scripts/headless-gpu.sh)"
        )),
    }
}

/// Recursively print the accessibility tree: role, label, value, numeric.
fn dump_tree(node: &accesskit_consumer::Node<'_>, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let mut line = format!("{indent}{:?}", node.role());
    if let Some(l) = node.label() {
        line.push_str(&format!(" label={l:?}"));
    }
    if let Some(v) = node.value() {
        line.push_str(&format!(" value={v:?}"));
    }
    if let Some(n) = node.numeric_value() {
        line.push_str(&format!(" numeric={n}"));
    }
    out.push_str(&line);
    out.push('\n');
    for child in node.children() {
        dump_tree(&child, depth + 1, out);
    }
}

fn arg(args: &[String], i: usize, usage: &str) -> Result<String, String> {
    args.get(i)
        .cloned()
        .ok_or_else(|| format!("usage: {usage}"))
}

/// Split a line into tokens, honouring double-quotes around labels with spaces.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in line.chars() {
        match c {
            '"' => in_quote = !in_quote,
            ' ' | '\t' if !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_key(name: &str) -> Option<egui::Key> {
    use egui::Key;
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        s if s.len() == 1 => {
            let ch = s.chars().next().unwrap();
            match ch {
                'a'..='z' => Key::from_name(&ch.to_ascii_uppercase().to_string())?,
                '0'..='9' => Key::from_name(&ch.to_string())?,
                _ => return None,
            }
        }
        _ => return None,
    })
}
