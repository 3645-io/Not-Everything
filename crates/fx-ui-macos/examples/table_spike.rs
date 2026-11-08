//! The Phase 0 UI spike: an AppKit window with a search field, a view-based
//! `NSTableView` over 2M virtual rows and a status bar, measured for the
//! Phase 0 gate (scroll frame time, launch to first paint). It needs the
//! `measure` feature:
//!
//! ```text
//! cargo run --release -p fx-ui-macos --features measure --example table_spike
//! cargo run --release -p fx-ui-macos --features measure --example table_spike -- --autoscroll 600
//! cargo run --release -p fx-ui-macos --features measure --example table_spike -- --launch-probe --json
//! ```
//!
//! See `fx_ui_macos::spike_args::USAGE` for all options. Bad options print
//! the usage text and exit with code 2. On hosts other than macOS the
//! example prints "macOS only" and exits 0.
//!
//! In `--autoscroll` and `--launch-probe` modes a watchdog exits with code
//! 1 if the report has not been printed in time (for example when no window
//! server is available), so the measurement never hangs.
//!
//! The spike is read-only: no open, reveal, delete, rename, move or drag
//! actions.

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};

    use fx_ui_macos::appkit::spike::{SpikeConfig, run};
    use fx_ui_macos::model::RowModel;
    use fx_ui_macos::spike_args::{SpikeArgs, USAGE};

    // As early as possible: the launch probe measures from here.
    let process_start = Instant::now();

    let args = match SpikeArgs::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("table_spike: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if args.launch_probe || args.autoscroll > 0 {
        let limit = Duration::from_secs(60)
            + Duration::from_millis(100)
                .saturating_mul(u32::try_from(args.autoscroll).unwrap_or(u32::MAX));
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            eprintln!(
                "table_spike: no report after {} s (is a window server available?)",
                limit.as_secs()
            );
            std::process::exit(1);
        });
    }

    let generate = Instant::now();
    let model = RowModel::new(args.rows, args.seed);
    let model_time = generate.elapsed();

    let config = SpikeConfig {
        args,
        model,
        process_start,
        model_time,
    };
    if let Err(e) = run(config) {
        eprintln!("table_spike: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("table_spike: macOS only");
}
