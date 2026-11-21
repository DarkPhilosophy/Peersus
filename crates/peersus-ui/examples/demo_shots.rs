//! Draws every screen of the read-only demo into PNG files, with no display.
//!
//! The pictures come from the same headless window the screen tests use
//! (`peersus_ui::headless`): the real components, layout and drawing code on the demo's
//! synthetic fixture, drawn by the CPU, with every animation at rest. They are for looking at a
//! change before and after, and for a pull request; they are not a photograph of the desktop
//! window (no GPU surface, no window system), and every device and name in them is made up.
//!
//! Run it:
//!     cargo run -p peersus-ui --example demo_shots --features headless -- OUT_DIR
//! Options:
//!     --texts    also print what each screen reads, one line per text

use peersus_ui::headless::{DESKTOP, NARROW, Window};
use std::path::PathBuf;

const PAGES: [&str; 5] = ["send", "receive", "peers", "transfers", "settings"];
/// The tabs of Settings, by the words their sidebar entries read.
const TABS: [&str; 6] = [
    "Device",
    "Appearance",
    "Network",
    "Extensions",
    "Permissions",
    "About",
];
/// Upright and tall enough that the compact form's Settings, which is one page of every
/// section, is drawn without scrolling: for reading it end to end, not a size any window
/// opens at.
const WHOLE: (f32, f32) = (480., 7200.);

fn open(args: &[String], size: (f32, f32)) -> Window {
    match Window::open(args, size) {
        Ok(window) => window,
        Err(reason) => {
            eprintln!("demo_shots: {reason}");
            std::process::exit(1);
        }
    }
}

fn save(window: &mut Window, path: PathBuf, texts: bool) {
    if let Err(error) = window.save(&path) {
        eprintln!("demo_shots: cannot write {}: {error}", path.display());
        std::process::exit(1);
    }
    println!("{}", path.display());
    if texts {
        for text in window.texts() {
            println!("    {text}");
        }
    }
}

fn main() {
    let mut out = None;
    let mut texts = false;
    for argument in std::env::args().skip(1) {
        if argument == "--texts" {
            texts = true;
        } else if argument.starts_with("--") || out.is_some() {
            eprintln!("demo_shots takes one output directory and --texts, not {argument:?}");
            std::process::exit(2);
        } else {
            out = Some(PathBuf::from(argument));
        }
    }
    let Some(out) = out else {
        eprintln!("demo_shots: name the directory the pictures go to");
        std::process::exit(2);
    };
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("demo_shots: cannot create {}: {error}", out.display());
        std::process::exit(1);
    }
    for (layout, size) in [("desktop", DESKTOP), ("narrow", NARROW)] {
        for page in PAGES {
            let args = [
                format!("--demo-page={page}"),
                "--demo-peers=5".to_owned(),
                "--demo-transfers=8".to_owned(),
            ];
            let mut window = open(&args, size);
            save(&mut window, out.join(format!("{layout}-{page}.png")), texts);
        }
    }
    for tab in TABS {
        let mut window = open(&["--demo-page=settings".to_owned()], DESKTOP);
        if !window.click_text(tab) {
            eprintln!("demo_shots: Settings shows no {tab} tab");
            std::process::exit(1);
        }
        let name = format!("desktop-settings-{}.png", tab.to_lowercase());
        save(&mut window, out.join(name), texts);
    }
    let mut whole = open(&["--demo-page=settings".to_owned()], WHOLE);
    save(&mut whole, out.join("compact-settings-whole.png"), texts);
}
