//! A minimal `.app` bundle writer, for measuring the spike's bundle size
//! and packaging the app.
//!
//! [`write_app_bundle`] lays out
//!
//! ```text
//! <out_dir>/<name>.app/
//!     Contents/
//!         Info.plist
//!         MacOS/<executable file name>
//! ```
//!
//! by copying the executable. [`write_app_bundle_with_icon`] also
//! copies a [`BundleIcon`] into `Contents/Resources`; [`product_icon`] is the
//! app's checked-in icon. They only create new entries: they refuse a target
//! that already exists and never delete, rename or move anything, including
//! on failure (a partly written bundle is left in place).
//! [`dir_size`] sums the file sizes under a directory, for recording the
//! bundle size.

#![forbid(unsafe_code)]

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use fx_channel::Channel;

/// The third-party notices' file name in the product bundle's
/// `Contents/Resources`.
pub const NOTICES_FILE: &str = "THIRD-PARTY-NOTICES.md";

/// The `LSMinimumSystemVersion` written to `Info.plist`.
pub const MIN_MACOS_VERSION: &str = "13.0";

/// The `CFBundleVersion` written to `Info.plist`: the build number of the
/// package version, `X*10000 + Y*100 + Z`, so 0.2.0
/// is 200 ([`fx_channel::build_number`]). `CFBundleShortVersionString` is
/// the bare version. A version the rule can't number fails the build.
pub const BUILD_NUMBER: u32 = match fx_channel::build_number(env!("CARGO_PKG_VERSION")) {
    Some(n) => n,
    None => panic!("the workspace version must be X.Y.Z with the minor and patch parts below 100"),
};

/// An app icon for the bundle, as `actool` compiles an Icon Composer icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleIcon {
    /// The icon's name: the `.icns` file stem and the asset catalog's app
    /// icon (`CFBundleIconFile` and `CFBundleIconName`).
    pub name: String,
    /// The `.icns` file, used by macOS 13 to 15.
    pub icns: PathBuf,
    /// The compiled asset catalog (`Assets.car`) holding the icon with its
    /// appearances, used by macOS 26 and later.
    pub asset_catalog: PathBuf,
}

/// The app's icon: the approved Match Row design with its light and dark
/// artwork, compiled by `scripts/build-icons.sh` into the crate's
/// `assets/bundle/`.
pub fn product_icon() -> BundleIcon {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bundle");
    BundleIcon {
        name: "AppIcon".to_string(),
        icns: dir.join("AppIcon.icns"),
        asset_catalog: dir.join("Assets.car"),
    }
}

/// Creates `<out_dir>/<name>.app` holding a copy of `exe_path` and an
/// `Info.plist`, and returns the `.app` path.
///
/// The plist sets `CFBundleIdentifier` (`bundle_id`), `CFBundleExecutable`
/// (the executable's file name), `CFBundleName` (`name`),
/// `CFBundlePackageType` `APPL`, `CFBundleShortVersionString` (the package
/// version), `CFBundleVersion` ([`BUILD_NUMBER`]), `LSMinimumSystemVersion`
/// 13.0 and `NSHighResolutionCapable`. It does not set `LSUIElement`, which
/// would hide the app's Dock icon and its own menu bar.
///
/// # Errors
///
/// - `InvalidInput` if `name` or `bundle_id` contains "everything" in any
///   case (project naming rule), except exactly the release and Preview
///   apps' names and bundle IDs, the owner's choice
///   ([`fx_channel::Channel::app_name`] and
///   [`fx_channel::Channel::bundle_id`]); if `name` is empty or contains a
///   path separator or control character, or if `bundle_id` is not made of
///   ASCII letters, digits, `-` and `.`. Nothing is created.
/// - `AlreadyExists` if `<name>.app` already exists in any form; it is left
///   untouched.
/// - Any I/O error reading the executable or writing the bundle. An
///   executable that is missing or not a regular file is reported before
///   anything is created.
pub fn write_app_bundle(
    out_dir: &Path,
    exe_path: &Path,
    name: &str,
    bundle_id: &str,
) -> io::Result<PathBuf> {
    write_bundle(out_dir, exe_path, name, bundle_id, None)
}

/// The product layout: like [`write_app_bundle`], except that the
/// executable is named after the app (`Contents/MacOS/<name>`, as Mac apps
/// name it), and also copies `icon` into `Contents/Resources`
/// (`<icon name>.icns` and `Assets.car`) with the third-party notices
/// ([`NOTICES_FILE`]), and names the icon in the plist with
/// `CFBundleIconFile` and `CFBundleIconName`. Sign the result with
/// [`sign`] before shipping it.
///
/// # Errors
///
/// As [`write_app_bundle`], plus `InvalidInput` for an icon name that is
/// empty, `.` or `..`, or holds a path separator or control character, and
/// any I/O error reading the icon files. Bad or missing icon files are
/// reported before anything is created.
pub fn write_app_bundle_with_icon(
    out_dir: &Path,
    exe_path: &Path,
    name: &str,
    bundle_id: &str,
    icon: &BundleIcon,
) -> io::Result<PathBuf> {
    write_bundle(out_dir, exe_path, name, bundle_id, Some(icon))
}

fn write_bundle(
    out_dir: &Path,
    exe_path: &Path,
    name: &str,
    bundle_id: &str,
    icon: Option<&BundleIcon>,
) -> io::Result<PathBuf> {
    validate_name(name)?;
    validate_bundle_id(bundle_id)?;

    let exe_meta = fs::metadata(exe_path)?;
    if !exe_meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("executable {} is not a regular file", exe_path.display()),
        ));
    }
    let source_name = exe_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !has_control(n))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "executable name of {} must be valid UTF-8",
                    exe_path.display()
                ),
            )
        })?;
    // The product layout names the executable after the app.
    let exe_name = match icon {
        Some(icon) => {
            validate_icon(icon)?;
            name
        }
        None => source_name,
    };

    let app = out_dir.join(format!("{name}.app"));
    // `create_dir` fails with AlreadyExists for any existing entry
    // (directory, file or symlink), so an existing target is never touched.
    fs::create_dir(&app)?;
    let contents = app.join("Contents");
    fs::create_dir(&contents)?;
    let macos = contents.join("MacOS");
    fs::create_dir(&macos)?;

    let target_exe = macos.join(exe_name);
    // The directory is new and empty, so this cannot overwrite anything;
    // `fs::copy` keeps the permission bits, so the copy stays executable.
    fs::copy(exe_path, &target_exe)?;

    if let Some(icon) = icon {
        let resources = contents.join("Resources");
        fs::create_dir(&resources)?;
        // A new, empty directory again: nothing is overwritten.
        fs::copy(&icon.icns, resources.join(format!("{}.icns", icon.name)))?;
        fs::copy(&icon.asset_catalog, resources.join("Assets.car"))?;
        // The product carries the notices its open-source crates' licences
        // ask for; the binary holds them too, for the About panel.
        fs::write(
            resources.join(NOTICES_FILE),
            crate::notices::THIRD_PARTY_NOTICES,
        )?;
    }

    let plist = info_plist(name, bundle_id, exe_name, icon.map(|i| i.name.as_str()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(contents.join("Info.plist"))?;
    file.write_all(plist.as_bytes())?;
    file.flush()?;

    Ok(app)
}

/// Who signs a bundle for [`sign`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signer<'a> {
    /// An ad hoc signature (`codesign --sign -`), with no certificate: for
    /// tests, CI and local builds. Gatekeeper asks the user to approve a
    /// downloaded copy (Privacy & Security, Open Anyway), and macOS ties
    /// Full Disk Access to the build's code hash, so an update loses it.
    AdHoc,
    /// A code-signing identity in the keychain, named as `codesign --sign`
    /// takes it: the certificate's name, such as "Developer ID Application:
    /// Name (TEAMID)", or its SHA-1 hash. Releases are signed with the
    /// owner's Developer ID, so they can be notarized and macOS ties Full
    /// Disk Access to the team ID.
    Identity(&'a str),
}

/// Signs the bundle at `app` (`codesign --force`), sealing its executable,
/// `Info.plist` and resources, then checks it with
/// `codesign --verify --deep --strict`. macOS only.
///
/// Every signature turns on the hardened runtime (`--options runtime`),
/// which notarization requires. The app needs none of the
/// exceptions it can grant, so the bundle has no entitlements. A
/// [`Signer::Identity`] signature also gets a secure timestamp from Apple
/// (`--timestamp`, which needs the network); an ad hoc one has none.
///
/// Without a signature a bundle carries only the executable's linker
/// signature, which does not cover the bundle: `codesign` rejects it ("code
/// has no resources but signature indicates they must be present"), and
/// Gatekeeper reports a downloaded copy as damaged.
///
/// # Errors
///
/// `InvalidInput`, before `codesign` runs, for an identity that is blank or
/// `-` (ad hoc is [`Signer::AdHoc`]). An I/O error if `codesign` cannot run,
/// or `Other` with its message if signing or verification fails (for
/// example a missing bundle or an identity the keychain does not hold).
#[cfg(target_os = "macos")]
pub fn sign(app: &Path, signer: Signer<'_>) -> io::Result<()> {
    if let Signer::Identity(name) = signer {
        if matches!(name.trim(), "" | "-") {
            return Err(invalid(format!(
                "signing identity {name:?} names no certificate; ad hoc is Signer::AdHoc"
            )));
        }
    }
    let codesign = |args: &[&str]| -> io::Result<()> {
        let out = std::process::Command::new("/usr/bin/codesign")
            .args(args)
            .arg(app)
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "codesign {} {}: {}",
                args.join(" "),
                app.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    };
    codesign(&sign_args(signer))?;
    codesign(&["--verify", "--deep", "--strict"])
}

/// Signs the bundle at `app` ad hoc: [`sign`] with [`Signer::AdHoc`].
///
/// # Errors
///
/// As [`sign`].
#[cfg(target_os = "macos")]
pub fn sign_ad_hoc(app: &Path) -> io::Result<()> {
    sign(app, Signer::AdHoc)
}

/// The `codesign` arguments that sign for `signer`, before the bundle path.
#[cfg(target_os = "macos")]
fn sign_args(signer: Signer<'_>) -> Vec<&str> {
    let mut args = vec!["--force", "--options", "runtime"];
    match signer {
        Signer::AdHoc => args.extend(["--timestamp=none", "--sign", "-"]),
        Signer::Identity(name) => args.extend(["--timestamp", "--sign", name]),
    }
    args
}

/// Total size in bytes of the files under `path` (or of `path` itself if it
/// is a file). Symbolic links are not followed; each counts its own size.
///
/// # Errors
///
/// Any I/O error, including a missing `path`.
pub fn dir_size(path: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let meta = fs::symlink_metadata(&p)?;
        if meta.is_dir() {
            for entry in fs::read_dir(&p)? {
                stack.push(entry?.path());
            }
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

/// The XML property list for the bundle; `icon` is the icon's name, if it
/// has one.
fn info_plist(name: &str, bundle_id: &str, exe_name: &str, icon: Option<&str>) -> String {
    let string = |key: &str, value: &str| {
        format!(
            "\t<key>{key}</key>\n\t<string>{}</string>\n",
            xml_escape(value)
        )
    };
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    s.push_str("<plist version=\"1.0\">\n<dict>\n");
    s.push_str(&string("CFBundleIdentifier", bundle_id));
    s.push_str(&string("CFBundleExecutable", exe_name));
    s.push_str(&string("CFBundleName", name));
    s.push_str(&string("CFBundlePackageType", "APPL"));
    s.push_str(&string("CFBundleInfoDictionaryVersion", "6.0"));
    s.push_str(&string(
        "CFBundleShortVersionString",
        env!("CARGO_PKG_VERSION"),
    ));
    s.push_str(&string("CFBundleVersion", &BUILD_NUMBER.to_string()));
    s.push_str(&string("LSMinimumSystemVersion", MIN_MACOS_VERSION));
    s.push_str("\t<key>NSHighResolutionCapable</key>\n\t<true/>\n");
    if let Some(icon) = icon {
        s.push_str(&string("CFBundleIconFile", icon));
        s.push_str(&string("CFBundleIconName", icon));
    }
    s.push_str("</dict>\n</plist>\n");
    s
}

/// Returns `s` as a quoted JSON string literal, for `make_bundle --json`.
///
/// Escapes `"` and `\`, writes `\n`, `\r` and `\t` in their short form and
/// every other control character (U+0000 to U+001F and U+007F to U+009F) as
/// `\u00XX`. Everything else, including non-ASCII text, is kept as UTF-8.
pub fn json_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escapes the XML special characters in `s`.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Whether `s` contains "everything" ignoring ASCII case. Apart from the
/// owner's chosen names and bundle IDs of the release app and the Preview
/// app ([`allowed_name`], [`allowed_bundle_id`]), the project never ships
/// under that word.
fn mentions_everything(s: &str) -> bool {
    s.to_ascii_lowercase().contains("everything")
}

/// Whether `name` is exactly a channel's app name: Not-Everything or
/// Not-Everything Preview, in a build of any channel.
fn allowed_name(name: &str) -> bool {
    Channel::ALL.iter().any(|c| c.app_name() == name)
}

/// Whether `id` is exactly a channel's bundle ID: `io.3645.not-everything`
/// or `io.3645.not-everything.preview`.
fn allowed_bundle_id(id: &str) -> bool {
    Channel::ALL.iter().any(|c| c.bundle_id() == id)
}

fn validate_name(name: &str) -> io::Result<()> {
    if mentions_everything(name) && !allowed_name(name) {
        return Err(invalid(format!(
            "bundle name {name:?} must not contain \"everything\""
        )));
    }
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || has_control(name)
    {
        return Err(invalid(format!("invalid bundle name {name:?}")));
    }
    Ok(())
}

fn validate_icon(icon: &BundleIcon) -> io::Result<()> {
    let name = &icon.name;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || has_control(name)
    {
        return Err(invalid(format!("invalid icon name {name:?}")));
    }
    for file in [&icon.icns, &icon.asset_catalog] {
        if !fs::metadata(file)?.is_file() {
            return Err(invalid(format!(
                "icon file {} is not a regular file",
                file.display()
            )));
        }
    }
    Ok(())
}

fn validate_bundle_id(id: &str) -> io::Result<()> {
    if mentions_everything(id) && !allowed_bundle_id(id) {
        return Err(invalid(format!(
            "bundle identifier {id:?} must not contain \"everything\""
        )));
    }
    let ok_chars = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if id.is_empty() || !ok_chars {
        return Err(invalid(format!(
            "bundle identifier {id:?} may hold only ASCII letters, digits, '-' and '.'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids_are_validated() {
        assert!(validate_name("FxSpike").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_bundle_id("io.3645.spike").is_ok());
        assert!(validate_bundle_id("dev fx").is_err());
        assert!(validate_bundle_id("").is_err());
    }

    #[test]
    fn json_string_escapes_quotes_backslashes_and_controls() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(
            json_string("/tmp/a\"b\\c\td\u{e9}\u{1f600}.app"),
            "\"/tmp/a\\\"b\\\\c\\td\u{e9}\u{1f600}.app\""
        );
        assert_eq!(json_string("\n\r"), "\"\\n\\r\"");
        assert_eq!(json_string("\u{0}\u{1}\u{1f}"), "\"\\u0000\\u0001\\u001f\"");
        assert_eq!(json_string("\u{7f}\u{9f}"), "\"\\u007f\\u009f\"");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ad_hoc_signing_turns_on_the_runtime_without_a_timestamp() {
        assert_eq!(
            sign_args(Signer::AdHoc),
            [
                "--force",
                "--options",
                "runtime",
                "--timestamp=none",
                "--sign",
                "-"
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn identity_signing_asks_for_a_secure_timestamp() {
        let id = "Developer ID Application: A B (TEAM123456)";
        assert_eq!(
            sign_args(Signer::Identity(id)),
            [
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--sign",
                id
            ]
        );
    }

    #[test]
    fn exactly_the_channel_names_are_allowed() {
        assert!(allowed_name("Not-Everything"));
        assert!(allowed_name("Not-Everything Preview"));
        assert!(!allowed_name("Not-Everything Beta"));
        assert!(allowed_bundle_id("io.3645.not-everything"));
        assert!(allowed_bundle_id("io.3645.not-everything.preview"));
        assert!(!allowed_bundle_id("io.3645.not-everything.beta"));
    }

    #[test]
    fn plist_values_are_escaped() {
        let p = info_plist("A&B", "io.3645.spike", "x<y", Some("I&J"));
        assert!(p.contains("<string>A&amp;B</string>"));
        assert!(p.contains("<string>x&lt;y</string>"));
        assert!(p.contains("<string>I&amp;J</string>"));
    }
}
