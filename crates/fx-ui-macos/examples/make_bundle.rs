//! Wraps an executable (normally the release `table_spike`) in a minimal
//! `.app` bundle and prints the bundle's size, for the Phase 0 gate's `.app`
//! size number.
//!
//! ```text
//! cargo build --release -p fx-ui-macos --features measure --example table_spike
//! cargo run --release -p fx-ui-macos --example make_bundle -- \
//!     target/spike-bundle target/release/examples/table_spike
//! ```
//!
//! With `--app` it wraps the product app instead, as
//! `<OUT_DIR>/Not-Everything.app` with bundle ID `io.3645.not-everything`
//! (in a Preview build: `Not-Everything Preview.app`,
//! `io.3645.not-everything.preview`; see `fx_channel`) and the app icon from
//! `assets/bundle/` ([`fx_ui_macos::bundle::product_icon`]):
//!
//! ```text
//! cargo build --release -p fx-ui-macos
//! cargo run --release -p fx-ui-macos --example make_bundle -- \
//!     target/app-bundle target/release/fx-ui-macos --app
//! ```
//!
//! Otherwise it creates `<OUT_DIR>/Table Spike.app` with
//! [`fx_ui_macos::bundle::write_app_bundle`]. It signs either bundle ad
//! hoc so it passes `codesign --verify --strict`, or with
//! `--sign-identity NAME` with that keychain certificate instead
//! (releases use the owner's Developer ID, see `scripts/release-macos.sh`),
//! and every signature turns on the hardened runtime
//! ([`fx_ui_macos::bundle::sign`]). It refuses an existing bundle
//! and never deletes, renames or moves anything; pick a fresh `OUT_DIR` (or
//! remove the old bundle yourself) to measure again. Add `--json` for one
//! JSON object (with `--json`, `OUT_DIR` must be valid UTF-8). Exits 2 on
//! bad arguments and 1 on I/O errors.

use std::path::PathBuf;

use fx_ui_macos::bundle::{
    dir_size, json_string, product_icon, write_app_bundle, write_app_bundle_with_icon,
};
use fx_ui_macos::{APP_BUNDLE_ID, APP_NAME};

const USAGE: &str = "usage: make_bundle OUT_DIR EXECUTABLE [--app] [--sign-identity NAME] [--json]";
/// Bundle name and placeholder bundle ID of the spike.
const NAME: &str = "Table Spike";
const BUNDLE_ID: &str = "io.3645.table-spike";

fn main() {
    let mut json = false;
    let mut product = false;
    let mut identity: Option<String> = None;
    let mut paths = Vec::new();
    // `args_os`, so a non-UTF-8 path reaches the check below instead of
    // panicking.
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--json") => json = true,
            Some("--app") => product = true,
            Some("--sign-identity") => {
                let name = args
                    .next()
                    .and_then(|n| n.into_string().ok())
                    .filter(|n| !matches!(n.trim(), "" | "-"))
                    .unwrap_or_else(|| fail(2, "--sign-identity needs a certificate name"));
                identity = Some(name);
            }
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return;
            }
            Some(other) if other.starts_with('-') => fail(2, &format!("unknown option {other:?}")),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    let [out_dir, exe] = <[PathBuf; 2]>::try_from(paths)
        .unwrap_or_else(|_| fail(2, "expected OUT_DIR and EXECUTABLE"));
    // Only macOS signs, so refuse an identity before creating anything.
    #[cfg(not(target_os = "macos"))]
    if identity.is_some() {
        fail(2, "--sign-identity needs macOS");
    }
    // JSON strings must be valid UTF-8, so a non-UTF-8 output path is
    // rejected before anything is created. (The bundle and executable names
    // are UTF-8 already; `write_app_bundle` checks the executable's.)
    if json && out_dir.to_str().is_none() {
        fail(
            2,
            &format!("--json needs a UTF-8 OUT_DIR, got {}", out_dir.display()),
        );
    }

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        fail(1, &format!("cannot create {}: {e}", out_dir.display()));
    }
    let written = if product {
        write_app_bundle_with_icon(&out_dir, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
    } else {
        write_app_bundle(&out_dir, &exe, NAME, BUNDLE_ID)
    };
    let app = written.unwrap_or_else(|e| fail(1, &format!("cannot write the bundle: {e}")));
    // An unsigned bundle fails codesign verification, and Gatekeeper calls
    // a downloaded copy damaged.
    #[cfg(target_os = "macos")]
    {
        use fx_ui_macos::bundle::Signer;
        let signer = identity.as_deref().map_or(Signer::AdHoc, Signer::Identity);
        fx_ui_macos::bundle::sign(&app, signer)
            .unwrap_or_else(|e| fail(1, &format!("cannot sign the bundle: {e}")));
    }
    let bytes =
        dir_size(&app).unwrap_or_else(|e| fail(1, &format!("cannot size {}: {e}", app.display())));
    let exe_bytes = std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);

    if json {
        // UTF-8 by the check above; lossy conversion is only a fallback.
        let app_str = app.to_string_lossy();
        println!(
            "{{\"app\":{},\"app_bytes\":{bytes},\"executable_bytes\":{exe_bytes}}}",
            json_string(&app_str)
        );
    } else {
        println!("app: {}", app.display());
        println!(
            "app size: {bytes} bytes ({:.2} MiB)",
            bytes as f64 / (1024.0 * 1024.0)
        );
        println!("executable: {exe_bytes} bytes");
    }
}

fn fail(code: i32, message: &str) -> ! {
    eprintln!("make_bundle: {message}\n{USAGE}");
    std::process::exit(code);
}
