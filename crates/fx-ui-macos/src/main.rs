//! The `fx-ui-macos` app: the search window over an `fx_engine::Engine`.
//!
//! ```text
//! cargo run -p fx-ui-macos --release                       # index /
//! cargo run -p fx-ui-macos --release -- --root ~/Projects  # index a folder
//! ```
//!
//! The measurement modes need the `measure` feature, which release builds
//! leave off:
//!
//! ```text
//! cargo run -p fx-ui-macos --release --features measure -- --synthetic 2000000 --autoscroll
//! cargo run -p fx-ui-macos --release --features measure -- --synthetic 2000000 --launch-probe
//! cargo run -p fx-ui-macos --release --features measure -- --root /Volumes/Data --gate-probe ui.log
//! ```
//!
//! See `fx_ui_macos::app_args::USAGE`. Bad options print the usage text to
//! stderr and exit with code 2 before any window opens. On hosts other than
//! macOS the binary says it is macOS-only and exits 0.
//!
//! The app is read-only: Enter opens, Cmd+Enter reveals in Finder,
//! Option+Cmd+C copies full paths and dragging out offers copy and link
//! only.
//!
//! It runs as a resident menu-bar app (status item, global summon hotkey,
//! Launch at Login, Full Disk Access onboarding); closing the window hides
//! it and Quit comes from the status menu or Cmd+Q. The measurement modes
//! (`--autoscroll`, `--launch-probe`, `--gate-probe`) stay plain window
//! apps.

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Instant;

    use fx_ui_macos::APP_NAME;
    use fx_ui_macos::app_args::{AppArgs, USAGE};

    // As early as possible: the measurement modes time the launch from here.
    let process_start = Instant::now();

    let args = match AppArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{APP_NAME}: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = fx_ui_macos::appkit::app::launch(args, process_start) {
        eprintln!("{APP_NAME}: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    use fx_ui_macos::APP_NAME;
    println!("{APP_NAME}: this app is macOS-only (the window uses AppKit)");
}
