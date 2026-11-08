//! Contract tests for `fx_ui_macos::bundle`: the `.app` bundle writer
//! behind `make_bundle` (the app's bundle, and the spike's for measuring
//! its size), and `dir_size`.
//!
//! `write_app_bundle(out_dir, exe_path, name, bundle_id)` creates
//! `<out_dir>/<name>.app/Contents/{Info.plist, MacOS/<exe file name>}` by
//! copying the binary and returns the `.app` path. It refuses an existing
//! target and never deletes, renames or moves anything.
//!
//! `write_app_bundle_with_icon` also copies a `BundleIcon` (the `.icns` and
//! the compiled asset catalog) into `Contents/Resources` and names it in the
//! plist; `product_icon()` is the checked-in app icon. Neither sets
//! `LSUIElement`, so the app shows its Dock icon and menu bar. The icon
//! (product) bundle names its executable after the app, and on macOS
//! `sign_ad_hoc` signs a written bundle so it verifies (an unsigned bundle
//! is "damaged" to Gatekeeper). `sign` also signs with a certificate
//! (`Signer::Identity`, the owner's Developer ID for releases), and every
//! signature turns on the hardened runtime.
//!
//! `CFBundleShortVersionString` is the bare package version and
//! `CFBundleVersion` its build number (`X*10000 + Y*100 + Z`, 0.2.0 is 200).
//! The naming rule admits exactly the release app's and the Preview app's
//! names and bundle IDs, the owner's choice.
//!
//! Tests create and clean up their own directories under
//! `std::env::temp_dir()`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fx_channel::Channel;
use fx_ui_macos::bundle::{
    BundleIcon, dir_size, product_icon, write_app_bundle, write_app_bundle_with_icon,
};
#[cfg(target_os = "macos")]
use fx_ui_macos::bundle::{Signer, sign, sign_ad_hoc};
use fx_ui_macos::{APP_BUNDLE_ID, APP_NAME};

const EXE_BYTES: &[u8] = b"\xcf\xfa\xed\xfe fake mach-o executable for bundle tests";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-ui-macos-bundle-{tag}-{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Creates `<dir>/bin/<name>` holding `EXE_BYTES`, executable on Unix.
fn fake_exe(dir: &Path, name: &str) -> PathBuf {
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join(name);
    fs::write(&exe, EXE_BYTES).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    }
    exe
}

/// The value element right after `<key>KEY</key>` in an XML plist.
fn plist_value<'a>(plist: &'a str, key: &str) -> &'a str {
    let tag = format!("<key>{key}</key>");
    let start = plist
        .find(&tag)
        .unwrap_or_else(|| panic!("missing {tag} in:\n{plist}"))
        + tag.len();
    let rest = plist[start..].trim_start();
    let end = if rest.starts_with("<true/>") || rest.starts_with("<false/>") {
        rest.find("/>").unwrap() + 2
    } else {
        let close = rest
            .find("</")
            .unwrap_or_else(|| panic!("unterminated value for {key}"));
        close + rest[close..].find('>').unwrap() + 1
    };
    &rest[..end]
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn writes_the_standard_app_layout() {
    let tmp = TempDir::new("layout");
    let exe = fake_exe(&tmp.path, "table_spike");
    let out = tmp.path.join("out");
    fs::create_dir_all(&out).unwrap();

    let app = write_app_bundle(&out, &exe, "FxSpike", "io.3645.spike").unwrap();
    assert_eq!(app, out.join("FxSpike.app"));
    assert!(app.is_dir());

    let contents = app.join("Contents");
    assert!(contents.join("Info.plist").is_file());
    let copied = contents.join("MacOS").join("table_spike");
    assert_eq!(fs::read(&copied).unwrap(), EXE_BYTES);

    // The source binary is copied, not moved.
    assert_eq!(fs::read(&exe).unwrap(), EXE_BYTES);
}

#[cfg(unix)]
#[test]
fn copied_executable_stays_executable() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new("mode");
    let exe = fake_exe(&tmp.path, "table_spike");
    let app = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap();
    let mode = fs::metadata(app.join("Contents/MacOS/table_spike"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "mode {mode:o} is not executable");
}

#[test]
fn info_plist_holds_the_required_keys() {
    let tmp = TempDir::new("plist");
    let exe = fake_exe(&tmp.path, "table_spike");
    let app = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap();
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();

    assert!(plist.trim_start().starts_with("<?xml"), "{plist}");
    assert!(plist.contains("<plist version=\"1.0\">"), "{plist}");
    assert!(
        plist.contains("<dict>") && plist.contains("</dict>"),
        "{plist}"
    );
    assert!(plist.trim_end().ends_with("</plist>"), "{plist}");

    assert_eq!(
        plist_value(&plist, "CFBundleIdentifier"),
        "<string>io.3645.spike</string>"
    );
    assert_eq!(
        plist_value(&plist, "CFBundleExecutable"),
        "<string>table_spike</string>"
    );
    assert_eq!(
        plist_value(&plist, "CFBundleName"),
        "<string>FxSpike</string>"
    );
    assert_eq!(
        plist_value(&plist, "CFBundlePackageType"),
        "<string>APPL</string>"
    );
    assert_eq!(
        plist_value(&plist, "LSMinimumSystemVersion"),
        "<string>13.0</string>"
    );
    assert_eq!(plist_value(&plist, "NSHighResolutionCapable"), "<true/>");
}

/// `X.Y.Z` to `X*10000 + Y*100 + Z`, written out here rather than taken from
/// the crate under test.
fn expected_build_number(version: &str) -> u32 {
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    assert_eq!(parts.len(), 3, "{version}");
    assert!(parts[1] < 100 && parts[2] < 100, "{version}");
    parts[0] * 10_000 + parts[1] * 100 + parts[2]
}

/// The short version is the bare package version, and the bundle version
/// is its build number (0.2.0 is 200), in both layouts.
#[test]
fn info_plist_carries_the_version_and_build_number() {
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(expected_build_number("0.2.0"), 200);
    let tmp = TempDir::new("version");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let icon = fake_icon(&tmp.path);
    for dir in ["plain", "with-icon"] {
        fs::create_dir_all(tmp.path.join(dir)).unwrap();
    }
    let plain = write_app_bundle(
        &tmp.path.join("plain"),
        &exe,
        "ExampleApp",
        "dev.example.app",
    );
    let with_icon = write_app_bundle_with_icon(
        &tmp.path.join("with-icon"),
        &exe,
        "ExampleApp",
        "dev.example.app",
        &icon,
    );
    for app in [plain, with_icon] {
        let plist = fs::read_to_string(app.unwrap().join("Contents/Info.plist")).unwrap();
        assert_eq!(
            plist_value(&plist, "CFBundleShortVersionString"),
            format!("<string>{version}</string>")
        );
        assert_eq!(
            plist_value(&plist, "CFBundleVersion"),
            format!("<string>{}</string>", expected_build_number(version))
        );
    }
}

/// The app shows a Dock icon and its own menu bar, so the plist must not
/// mark it as an agent (`LSUIElement`), which hides both, or a
/// background-only app.
#[test]
fn info_plist_does_not_hide_the_dock_icon_or_menu_bar() {
    let tmp = TempDir::new("dock");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let icon = fake_icon(&tmp.path);
    for dir in ["plain", "with-icon"] {
        fs::create_dir_all(tmp.path.join(dir)).unwrap();
    }
    let plain = write_app_bundle(
        &tmp.path.join("plain"),
        &exe,
        "ExampleApp",
        "dev.example.app",
    );
    let with_icon = write_app_bundle_with_icon(
        &tmp.path.join("with-icon"),
        &exe,
        "ExampleApp",
        "dev.example.app",
        &icon,
    );
    for app in [plain, with_icon] {
        let plist = fs::read_to_string(app.unwrap().join("Contents/Info.plist")).unwrap();
        assert!(!plist.contains("LSUIElement"), "{plist}");
        assert!(!plist.contains("LSBackgroundOnly"), "{plist}");
    }
}

/// Every key in the plist appears once, so the icon keys and the others are
/// not duplicated (a duplicate makes the plist ambiguous to Launch
/// Services).
#[test]
fn info_plist_keys_are_unique() {
    let tmp = TempDir::new("unique-keys");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let icon = fake_icon(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, "ExampleApp", "dev.example.app", &icon)
        .unwrap();
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();

    let mut keys: Vec<&str> = plist
        .split("<key>")
        .skip(1)
        .map(|rest| &rest[..rest.find("</key>").expect("unterminated <key>")])
        .collect();
    assert!(keys.contains(&"CFBundleIconName"), "{plist}");
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), total, "duplicate keys in:\n{plist}");
}

/// Writes a fake `.icns` and asset catalog under `<dir>/icon/`.
fn fake_icon(dir: &Path) -> BundleIcon {
    let src = dir.join("icon");
    fs::create_dir_all(&src).unwrap();
    let icns = src.join("AppIcon.icns");
    let car = src.join("Assets.car");
    fs::write(&icns, ICNS_BYTES).unwrap();
    fs::write(&car, CAR_BYTES).unwrap();
    BundleIcon {
        name: "AppIcon".to_string(),
        icns,
        asset_catalog: car,
    }
}

const ICNS_BYTES: &[u8] = b"icns fake icon family";
const CAR_BYTES: &[u8] = b"BOMStore fake asset catalog";

/// The product bundle carries the app icon: the legacy `.icns`
/// (macOS 13 to 15) and the compiled asset catalog with the Icon Composer
/// icon (macOS 26 and later), named by `CFBundleIconFile` and
/// `CFBundleIconName`.
#[test]
fn icon_bundle_copies_the_icon_into_resources() {
    let tmp = TempDir::new("icon");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let icon = fake_icon(&tmp.path);
    let out = tmp.path.join("out");
    fs::create_dir_all(&out).unwrap();
    let app =
        write_app_bundle_with_icon(&out, &exe, "ExampleApp", "dev.example.app", &icon).unwrap();

    let resources = app.join("Contents/Resources");
    assert_eq!(
        entries(&resources),
        vec![
            "AppIcon.icns".to_string(),
            "Assets.car".to_string(),
            "THIRD-PARTY-NOTICES.md".to_string()
        ]
    );
    // The product carries the notices its crates' licences ask for.
    assert_eq!(
        fs::read_to_string(resources.join("THIRD-PARTY-NOTICES.md")).unwrap(),
        fx_ui_macos::notices::THIRD_PARTY_NOTICES
    );
    assert_eq!(
        fs::read(resources.join("AppIcon.icns")).unwrap(),
        ICNS_BYTES
    );
    assert_eq!(fs::read(resources.join("Assets.car")).unwrap(), CAR_BYTES);
    // The sources are copied, not moved.
    assert_eq!(fs::read(&icon.icns).unwrap(), ICNS_BYTES);
    assert_eq!(fs::read(&icon.asset_catalog).unwrap(), CAR_BYTES);

    let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    assert_eq!(
        plist_value(&plist, "CFBundleIconFile"),
        "<string>AppIcon</string>"
    );
    assert_eq!(
        plist_value(&plist, "CFBundleIconName"),
        "<string>AppIcon</string>"
    );
    // The product layout names the executable after the app.
    assert_eq!(
        plist_value(&plist, "CFBundleExecutable"),
        "<string>ExampleApp</string>"
    );
    assert_eq!(
        fs::read(app.join("Contents/MacOS/ExampleApp")).unwrap(),
        EXE_BYTES,
        "the executable is copied under the app's name"
    );
    assert_eq!(
        entries(&app.join("Contents/MacOS")),
        vec!["ExampleApp".to_string()]
    );
    assert_eq!(
        entries(&app.join("Contents")),
        vec![
            "Info.plist".to_string(),
            "MacOS".to_string(),
            "Resources".to_string()
        ]
    );
}

#[test]
fn plain_bundle_has_no_icon() {
    let tmp = TempDir::new("no-icon");
    let exe = fake_exe(&tmp.path, "table_spike");
    let app = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap();
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    assert!(!plist.contains("CFBundleIcon"), "{plist}");
    assert!(!app.join("Contents/Resources").exists());
}

#[test]
fn missing_icon_file_is_an_error_and_creates_nothing() {
    let tmp = TempDir::new("icon-missing");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let mut icon = fake_icon(&tmp.path);
    icon.asset_catalog = tmp.path.join("icon/missing.car");
    let out = tmp.path.join("out");
    fs::create_dir_all(&out).unwrap();
    assert!(
        write_app_bundle_with_icon(&out, &exe, "ExampleApp", "dev.example.app", &icon).is_err()
    );
    assert!(entries(&out).is_empty(), "{:?}", entries(&out));
}

#[test]
fn invalid_icon_names_are_rejected() {
    let tmp = TempDir::new("icon-name");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    let out = tmp.path.join("out");
    fs::create_dir_all(&out).unwrap();
    for bad in ["", "a/b", "..", "App\nIcon"] {
        let mut icon = fake_icon(&tmp.path);
        icon.name = bad.to_string();
        let err = write_app_bundle_with_icon(&out, &exe, "ExampleApp", "dev.example.app", &icon)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "{bad:?}: {err}");
    }
    assert!(entries(&out).is_empty());
}

/// The checked-in product icon, compiled by `scripts/build-icons.sh` from
/// the approved Match Row design (light and dark artwork).
#[test]
fn product_icon_files_are_checked_in() {
    let icon = product_icon();
    assert_eq!(icon.name, "AppIcon");
    let icns = fs::read(&icon.icns).unwrap();
    assert_eq!(&icns[..4], b"icns", "{}", icon.icns.display());
    let car = fs::read(&icon.asset_catalog).unwrap();
    assert_eq!(&car[..8], b"BOMStore", "{}", icon.asset_catalog.display());
}

#[test]
fn refuses_an_existing_target_and_leaves_it_untouched() {
    let tmp = TempDir::new("exists");
    let exe = fake_exe(&tmp.path, "table_spike");
    let existing = tmp.path.join("FxSpike.app");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("marker"), b"keep me").unwrap();

    let err = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");

    assert_eq!(entries(&existing), vec!["marker".to_string()]);
    assert_eq!(fs::read(existing.join("marker")).unwrap(), b"keep me");
}

#[test]
fn refuses_an_existing_file_at_the_target_path() {
    let tmp = TempDir::new("exists-file");
    let exe = fake_exe(&tmp.path, "table_spike");
    let existing = tmp.path.join("FxSpike.app");
    fs::write(&existing, b"a plain file").unwrap();

    let err = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");
    assert_eq!(fs::read(&existing).unwrap(), b"a plain file");
}

#[test]
fn second_write_to_the_same_place_is_refused() {
    let tmp = TempDir::new("twice");
    let exe = fake_exe(&tmp.path, "table_spike");
    let app = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap();
    let plist_before = fs::read(app.join("Contents/Info.plist")).unwrap();

    let err = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.other").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AlreadyExists, "{err}");
    assert_eq!(
        fs::read(app.join("Contents/Info.plist")).unwrap(),
        plist_before
    );
}

#[test]
fn rejects_everything_in_name_or_bundle_id_in_any_case() {
    let tmp = TempDir::new("everything");
    let exe = fake_exe(&tmp.path, "table_spike");
    let out = tmp.path.join("out");
    fs::create_dir_all(&out).unwrap();

    let cases = [
        ("Everything", "io.3645.spike"),
        ("NotEverything", "io.3645.spike"),
        ("my EVERYTHING app", "io.3645.spike"),
        ("FxSpike", "com.voidtools.everything"),
        ("FxSpike", "dev.NotEveryThing.spike"),
        ("eVeRyThInG", "dev.EVERYTHING"),
        // Only the exact release and Preview names and bundle IDs are
        // allowed; near misses are not.
        ("not-everything", "io.3645.spike"),
        ("Not-Everything Pro", "io.3645.spike"),
        ("Not-Everything", "io.3645.not-everything.beta"),
        ("Not-Everything", "io.3645.Not-Everything"),
        ("Not-Everything preview", "io.3645.spike"),
        ("Not-Everything Preview 2", "io.3645.spike"),
        ("Not-Everything-Preview", "io.3645.spike"),
        ("NotEverything Preview", "io.3645.spike"),
        ("not-everything-preview", "io.3645.spike"),
        ("Not-Everything Preview", "io.3645.not-everything.Preview"),
        ("Not-Everything Preview", "io.3645.not-everything-preview"),
        (
            "Not-Everything Preview",
            "io.3645.not-everything.preview.beta",
        ),
        ("Not-Everything Preview", "io.3645.not-everything.rc"),
    ];
    for (name, id) in cases {
        let err = write_app_bundle(&out, &exe, name, id).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::InvalidInput,
            "name {name:?} id {id:?}: {err}"
        );
    }
    assert!(
        entries(&out).is_empty(),
        "nothing may be created on rejection"
    );
}

/// The owner named the product Not-Everything, bundle ID
/// io.3645.not-everything, and its release candidates and betas
/// Not-Everything Preview, bundle ID io.3645.not-everything.preview. Those
/// exact names are the exceptions to the rule, in every build.
#[test]
fn the_release_and_preview_names_and_bundle_ids_are_allowed() {
    let tmp = TempDir::new("product-name");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    for (name, id, dir) in [
        ("Not-Everything", "io.3645.not-everything", "release"),
        (
            "Not-Everything Preview",
            "io.3645.not-everything.preview",
            "preview",
        ),
    ] {
        let out = tmp.path.join(dir);
        fs::create_dir_all(&out).unwrap();
        let app = write_app_bundle_with_icon(&out, &exe, name, id, &product_icon()).unwrap();
        assert_eq!(app, out.join(format!("{name}.app")));
        assert!(app.join("Contents/MacOS").join(name).is_file(), "{name}");
        let plist = fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert_eq!(
            plist_value(&plist, "CFBundleName"),
            format!("<string>{name}</string>")
        );
        assert_eq!(
            plist_value(&plist, "CFBundleIdentifier"),
            format!("<string>{id}</string>")
        );
        assert_eq!(
            plist_value(&plist, "CFBundleExecutable"),
            format!("<string>{name}</string>")
        );
    }
}

/// The names the bundle writer admits are the channels' names: the app
/// built for each channel can be bundled, whichever channel built the
/// bundle writer.
#[test]
fn every_channel_s_app_can_be_bundled() {
    let tmp = TempDir::new("channels");
    let exe = fake_exe(&tmp.path, "fx-ui-macos");
    for channel in Channel::ALL {
        let out = tmp.path.join(channel.as_str());
        fs::create_dir_all(&out).unwrap();
        let app = write_app_bundle(&out, &exe, channel.app_name(), channel.bundle_id());
        assert!(app.is_ok(), "{channel:?}: {app:?}");
    }
    assert_eq!(APP_NAME, Channel::CURRENT.app_name());
    assert_eq!(APP_BUNDLE_ID, Channel::CURRENT.bundle_id());
}

#[test]
fn missing_executable_is_an_error() {
    let tmp = TempDir::new("noexe");
    let missing = tmp.path.join("bin").join("does_not_exist");
    assert!(write_app_bundle(&tmp.path, &missing, "FxSpike", "io.3645.spike").is_err());
}

#[test]
fn dir_size_sums_file_sizes_recursively() {
    let tmp = TempDir::new("size");
    let root = tmp.path.join("tree");
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("top.bin"), vec![0u8; 100]).unwrap();
    fs::write(root.join("a/one.bin"), vec![1u8; 2_000]).unwrap();
    fs::write(root.join("a/b/two.bin"), vec![2u8; 30]).unwrap();
    fs::write(root.join("a/b/c/three.bin"), vec![3u8; 4_096]).unwrap();
    fs::write(root.join("a/b/c/zero.bin"), b"").unwrap();

    assert_eq!(dir_size(&root).unwrap(), 100 + 2_000 + 30 + 4_096);
    assert_eq!(dir_size(&root.join("a/b")).unwrap(), 30 + 4_096);
    assert_eq!(dir_size(&root.join("empty")).unwrap(), 0);
}

#[test]
fn dir_size_of_a_bundle_is_its_executable_plus_plist() {
    let tmp = TempDir::new("bundle-size");
    let exe = fake_exe(&tmp.path, "table_spike");
    let app = write_app_bundle(&tmp.path, &exe, "FxSpike", "io.3645.spike").unwrap();
    let plist_len = fs::metadata(app.join("Contents/Info.plist")).unwrap().len();
    assert_eq!(dir_size(&app).unwrap(), EXE_BYTES.len() as u64 + plist_len);
}

#[test]
fn dir_size_of_a_missing_path_is_an_error() {
    let tmp = TempDir::new("size-missing");
    assert!(dir_size(&tmp.path.join("nope")).is_err());
}

/// A real Mach-O executable to bundle: a copy of this test binary, which the
/// linker signed ad hoc like every arm64 build.
#[cfg(target_os = "macos")]
fn real_exe(dir: &Path) -> PathBuf {
    let exe = dir.join("bin").join("fx-ui-macos");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    exe
}

#[cfg(target_os = "macos")]
fn codesign(args: &[&str], app: &Path) -> std::process::Output {
    std::process::Command::new("/usr/bin/codesign")
        .args(args)
        .arg(app)
        .output()
        .expect("run codesign")
}

/// A release once shipped like this, so it stays a check: a bundle around
/// a linker-signed executable, never signed itself, fails verification
/// ("code has no resources but signature indicates they must be present"),
/// which Gatekeeper reports for a downloaded copy as "damaged".
#[cfg(target_os = "macos")]
#[test]
fn an_unsigned_bundle_fails_codesign_verification() {
    let tmp = TempDir::new("unsigned");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    let out = codesign(&["--verify", "--deep", "--strict"], &app);
    assert!(!out.status.success(), "{out:?}");
}

/// `sign_ad_hoc` signs the whole bundle, so it verifies strictly,
/// carries the bundle ID as its identifier and seals the Info.plist and
/// resources.
#[cfg(target_os = "macos")]
#[test]
fn a_signed_bundle_passes_strict_verification() {
    let tmp = TempDir::new("signed");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    sign_ad_hoc(&app).unwrap();

    let out = codesign(&["--verify", "--deep", "--strict"], &app);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let details = codesign(&["-dv"], &app);
    let text = String::from_utf8_lossy(&details.stderr);
    assert!(text.contains("Identifier=io.3645.not-everything"), "{text}");
    assert!(text.contains("Signature=adhoc"), "{text}");
    assert!(!text.contains("Info.plist=not bound"), "{text}");
    assert!(!text.contains("Sealed Resources=none"), "{text}");
}

#[cfg(target_os = "macos")]
#[test]
fn signing_a_missing_bundle_is_an_error() {
    let tmp = TempDir::new("sign-missing");
    assert!(sign_ad_hoc(&tmp.path.join("Nope.app")).is_err());
}

/// The `CodeDirectory` line of `codesign -dv` output, which lists the
/// signature's flags.
#[cfg(target_os = "macos")]
fn code_directory_line(details: &str) -> &str {
    details
        .lines()
        .find(|l| l.starts_with("CodeDirectory"))
        .unwrap_or("")
}

/// Every signature turns on the hardened runtime, which
/// notarization requires. Ad hoc builds get it too, so tests, CI and local
/// builds run under the same restrictions as a release.
#[cfg(target_os = "macos")]
#[test]
fn an_ad_hoc_signature_turns_on_the_hardened_runtime() {
    let tmp = TempDir::new("runtime");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    sign(&app, Signer::AdHoc).unwrap();

    let details = codesign(&["-dv"], &app);
    let text = String::from_utf8_lossy(&details.stderr);
    assert!(text.contains("Signature=adhoc"), "{text}");
    assert!(code_directory_line(&text).contains("runtime"), "{text}");
}

/// An identity that is not in the keychain is an error carrying codesign's
/// message, and the bundle is left unsigned.
#[cfg(target_os = "macos")]
#[test]
fn signing_with_an_unknown_identity_is_an_error() {
    let tmp = TempDir::new("sign-unknown");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    let err = sign(
        &app,
        Signer::Identity("Developer ID Application: Nobody (0000000000)"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("codesign"), "{err}");
    let out = codesign(&["--verify", "--deep", "--strict"], &app);
    assert!(!out.status.success(), "{out:?}");
}

/// `Signer::Identity` never means ad hoc: a blank name or `-` (codesign's
/// ad hoc name) is refused before codesign runs, so a release can't be
/// signed ad hoc by mistake.
#[cfg(target_os = "macos")]
#[test]
fn a_blank_or_dash_identity_is_refused() {
    let tmp = TempDir::new("sign-blank");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    for name in ["", "  ", "-", " - "] {
        let err = sign(&app, Signer::Identity(name)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "{name:?}: {err}");
    }
    let out = codesign(&["--verify", "--deep", "--strict"], &app);
    assert!(!out.status.success(), "{out:?}");
}

/// The release check: a Developer ID signature carries Apple's
/// certificate chain, the team ID, a secure timestamp and the hardened
/// runtime, which notarization needs, and its designated requirement names
/// the team rather than one certificate, so Full Disk Access survives
/// updates and certificate renewals. CI has no certificate, so it runs only
/// on a Mac that has one:
///
/// ```text
/// NE_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
///     cargo test -p fx-ui-macos --test bundle -- --ignored
/// ```
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs a Developer ID certificate named in NE_SIGN_IDENTITY"]
fn a_developer_id_signature_is_ready_for_notarization() {
    let identity = std::env::var("NE_SIGN_IDENTITY")
        .expect("set NE_SIGN_IDENTITY to a Developer ID Application identity");
    let tmp = TempDir::new("developer-id");
    let exe = real_exe(&tmp.path);
    let app = write_app_bundle_with_icon(&tmp.path, &exe, APP_NAME, APP_BUNDLE_ID, &product_icon())
        .unwrap();
    sign(&app, Signer::Identity(&identity)).unwrap();

    let details = codesign(&["-dvv"], &app);
    let text = String::from_utf8_lossy(&details.stderr);
    assert!(text.contains("Identifier=io.3645.not-everything"), "{text}");
    assert!(
        text.contains("Authority=Developer ID Application:"),
        "{text}"
    );
    assert!(text.contains("Authority=Apple Root CA"), "{text}");
    assert!(text.contains("Timestamp="), "{text}");
    assert!(!text.contains("TeamIdentifier=not set"), "{text}");
    assert!(code_directory_line(&text).contains("runtime"), "{text}");

    let requirement = codesign(&["-d", "-r-"], &app);
    let req = format!(
        "{}{}",
        String::from_utf8_lossy(&requirement.stdout),
        String::from_utf8_lossy(&requirement.stderr)
    );
    assert!(req.contains("anchor apple generic"), "{req}");
    assert!(req.contains("certificate leaf[subject.OU]"), "{req}");
}
