//! macOS release script tests.
//!
//! `scripts/release-macos.sh` builds, signs, notarizes and packages a
//! release, a release candidate (`--rc`) or a beta (`--beta`). These tests
//! check that:
//!
//! - the script exists, uses `set -eu` and never deletes, renames or moves
//!   anything: no `rm`, `rmdir`, `unlink`, `mv` or `find -delete`; and it
//!   never moves or deletes a tag or a GitHub release;
//! - it notarizes in two uploads, in Apple's order for nested containers:
//!   the app and the command first, then the disk image built from the
//!   stapled app. Each upload goes through `upload`, which saves the
//!   submission (`for-notary.json`, `for-notary-dmg.json`) before waiting;
//!   the build's channel (`release-info`) is saved before the first upload.
//!   Before each staple it checks that Apple's ticket covers the signed
//!   items (their CDHash in the `notarytool log`);
//! - it makes a drag-to-install disk image (the app and a shortcut to
//!   /Applications, laid out by Finder or plain if Finder can't be
//!   scripted, read-only UDZO, signed with the Developer ID) in a fresh
//!   folder, and checks a quarantined copy of it (`hdiutil verify`,
//!   `spctl --assess --type open`, `stapler validate`, the app inside with
//!   `syspolicy_check distribution`) before writing the files; it detaches
//!   every image it attaches, and its `mount_point` helper reads `hdiutil
//!   attach`'s output;
//! - the disk image is written, summed and published next to the zip;
//! - the CLI tarball lists its files as root:wheel with no extended
//!   attributes, ACLs or file flags, and `check_tarball` stops at one with
//!   another owner, any pax extended header or AppleDouble `._` files;
//! - it checks the signed app's designated requirement (the bundle ID plus
//!   the team, which keeps Full Disk Access across builds) before the upload
//!   or, for a beta, before writing the files;
//! - its per-channel names are the `fx_channel` names, and it writes the
//!   release and Preview file names;
//! - it tags and publishes only with `--publish`, and only to the project's
//!   repository: its Plan prints `origin`, `--publish` refuses any other
//!   origin (`NE_RELEASE_REPO` names another for testing) and, for a release
//!   or an RC, a `gh` that is signed out or points elsewhere; only "release
//!   not found" counts as no GitHub release;
//! - a release or an RC checks the notary profile before building, and the
//!   build uses the locked dependencies (`--locked`);
//! - `--resume OUT_DIR` carries on with a folder an earlier run left while
//!   Apple was still processing an upload, at the stage it stopped in. On
//!   Unix, the script refuses before touching anything a folder that does
//!   not exist, one with no saved upload or channel record, a beta's, one
//!   missing the signed app or command (or, once the disk image was
//!   uploaded, that image), and one that already holds release files, and
//!   reports a usage error for a missing folder, an unknown option or two
//!   modes;
//! - in a throwaway git repository, each mode refuses the wrong branch, a
//!   dirty tree and the tag states the release flow rules out, and numbers
//!   release candidates and betas after the highest existing tag;
//! - `--perf PERF_DIR` (a `scripts/perf-check.sh --tier rc` run) takes one
//!   folder and no beta; `--publish` of a release candidate or a release
//!   needs it, `--resume` included. Before anything is fetched or built the
//!   script refuses a run without `metrics.txt`, `summary.md` or `run.txt`,
//!   of another tier or commit, with uncommitted changes, or whose last
//!   `perf-compare:` line has a failing or unmeasured metric, and a version
//!   whose previous release (the highest `release/vX.Y.Z` tag below it) has
//!   no `docs/gates/releases/<X.Y.Z>.txt`. The release build includes
//!   fx-bench, which is not shipped; once the disk image is final,
//!   `perf_section` writes `perf-metrics.txt` (with `dmg_bytes`) and runs
//!   `fx-bench perf-release`, and `release_notes` puts its table after the
//!   CHANGELOG.md entry.
//!
//! The behaviour tests never build, sign or contact Apple or GitHub: they
//! run with only the system folders on `PATH`, so a run that passes every
//! check stops at "cargo is not on PATH". Only `std` and `fx-channel` are
//! used.

mod common;

use common::shell::{
    code_lines, first_line_with, logical_lines, shell_function, strip_quoted, words,
};
use common::{read, repo_root};
use fx_channel::Channel;
use std::path::PathBuf;

/// Path of the release script, relative to the repository root.
const SCRIPT: &str = "scripts/release-macos.sh";

fn script_path() -> PathBuf {
    repo_root().join(SCRIPT)
}

fn script_text() -> String {
    let path = script_path();
    assert!(path.is_file(), "{SCRIPT} must exist (the release script)");
    read(&path)
}

/// The designated requirement of a Developer ID signature by the owner's
/// team, as `codesign -d -r-` prints it (checked on the owner's Mac), as the
/// script spells it inside a double-quoted string.
const REQUIREMENT: &str = concat!(
    r#"designated => identifier \"$BUNDLE_ID\" and anchor apple generic and "#,
    r#"certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and "#,
    r#"certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and "#,
    r#"certificate leaf[subject.OU] = $TEAM_ID"#,
);

// ---------------------------------------------------------------------------
// Script content.
// ---------------------------------------------------------------------------

#[test]
fn script_has_a_shell_shebang_and_strict_mode() {
    let text = script_text();
    let first = text.lines().next().unwrap_or("");
    assert!(
        first.starts_with("#!") && first.contains("sh"),
        "{SCRIPT} must start with a sh shebang, found {first:?}"
    );
    let strict = code_lines(&text)
        .iter()
        .any(|l| l.trim().starts_with("set -eu"));
    assert!(strict, "{SCRIPT} must run with `set -eu`");
}

#[test]
fn script_never_deletes_or_moves() {
    let text = script_text();
    let forbidden = ["rm", "rmdir", "unlink", "mv", "-delete", "srm"];
    let mut hits = Vec::new();
    for line in code_lines(&text) {
        let bare = strip_quoted(&line);
        for w in words(&bare) {
            if forbidden.contains(&w.as_str()) {
                hits.push(line.trim().to_owned());
            }
        }
    }
    assert!(
        hits.is_empty(),
        "{SCRIPT} must never delete or move (no rm/rmdir/unlink/mv/-delete): {hits:?}"
    );
}

/// "Never delete a release or a tag" (`.agents/rules/branches-and-releases.md`): no forced or deleting
/// git tag or push, and no `gh release` command but `create` and `view`.
#[test]
fn script_never_moves_or_deletes_tags_or_releases() {
    let mut hits = Vec::new();
    for line in code_lines(&script_text()) {
        let bare = strip_quoted(&line);
        let w = words(&bare);
        let has = |s: &str| w.iter().any(|x| x == s);
        let git_tag_or_push = w
            .windows(2)
            .any(|p| p[0] == "git" && (p[1] == "tag" || p[1] == "push"))
            || (has("git") && (has("tag") || has("push")));
        if git_tag_or_push
            && [
                "-d",
                "-f",
                "--delete",
                "--force",
                "--force-with-lease",
                "--mirror",
            ]
            .iter()
            .any(|f| has(f))
        {
            hits.push(line.trim().to_owned());
        }
        if git_tag_or_push
            && w.iter()
                .any(|x| x.starts_with(':') || x.starts_with("+refs"))
        {
            hits.push(line.trim().to_owned());
        }
        if let Some(i) = w.windows(2).position(|p| p[0] == "gh" && p[1] == "release") {
            let sub = w.get(i + 2).map(String::as_str);
            if !matches!(sub, Some("create" | "view")) || has("--clobber") {
                hits.push(line.trim().to_owned());
            }
        }
    }
    assert!(
        hits.is_empty(),
        "{SCRIPT} must never move or delete a tag or a release: {hits:?}"
    );
}

/// The first upload: the zip holding the signed app and command.
const APP_UPLOAD: &str = r#"upload "$NOTARY.zip" "$NOTARY""#;
/// The second upload: the disk image itself.
const DMG_UPLOAD: &str = r#"upload "$DMG" "$NOTARY_DMG""#;

/// The index of the first code line that is exactly `call` (trimmed), or a
/// panic naming `what`.
fn call_line(code: &[String], call: &str, what: &str) -> usize {
    code.iter()
        .position(|l| l.trim() == call)
        .unwrap_or_else(|| panic!("{SCRIPT} must {what} (`{call}`)"))
}

/// Every upload goes through `upload`, which saves Apple's answer as the
/// submission record (`<record>.json`, what `--resume` reads) only once the
/// upload has worked, and never waits; `wait_for` waits with `notarytool
/// wait`.
#[test]
fn script_uploads_through_one_function_and_saves_each_submission() {
    let text = script_text();
    let code = code_lines(&text);
    let submits = code
        .iter()
        .filter(|l| l.contains("notarytool submit"))
        .count();
    assert_eq!(submits, 1, "{SCRIPT} must upload only in `upload`");
    let upload =
        shell_function(&text, "upload").unwrap_or_else(|| panic!("{SCRIPT} must define upload"));
    assert!(upload.contains("notarytool submit") && !upload.contains("--wait"));
    let submit = upload.find("notarytool submit").unwrap();
    let saved = upload
        .find(r#"cp "$2.out" "$2.json""#)
        .unwrap_or_else(|| panic!("upload must save the submission record as <record>.json"));
    assert!(
        submit < saved,
        "upload must save the record after the upload"
    );
    let wait = shell_function(&text, "wait_for")
        .unwrap_or_else(|| panic!("{SCRIPT} must define wait_for"));
    assert!(wait.contains("notarytool wait") && wait.contains(r#""$1.json""#));
    call_line(&code, APP_UPLOAD, "upload the app and the command");
    call_line(&code, DMG_UPLOAD, "upload the disk image");
}

/// Apple's documented order for nested containers, which the first RC
/// showed is needed: `syspolicy_check distribution` wants the app inside
/// the disk image stapled. So the app and the command go first, the app is
/// stapled, and only then is the disk image built from it, notarized on its
/// own and stapled, before the checks and the files.
#[test]
fn script_notarizes_the_app_then_the_disk_image_built_from_it() {
    let code = code_lines(&script_text());
    let steps = [
        call_line(&code, APP_UPLOAD, "upload the app and the command first"),
        call_line(&code, r#"wait_for "$NOTARY""#, "wait for the first upload"),
        call_line(
            &code,
            r#"check_ticket "$NOTARY" "$APP" "$CLI""#,
            "check the first ticket covers the app and the command",
        ),
        call_line(&code, r#"xcrun stapler staple "$APP""#, "staple the app"),
        call_line(&code, "make_dmg", "build the disk image"),
        call_line(&code, DMG_UPLOAD, "upload the disk image second"),
        call_line(
            &code,
            r#"wait_for "$NOTARY_DMG""#,
            "wait for the second upload",
        ),
        call_line(
            &code,
            r#"check_ticket "$NOTARY_DMG" "$DMG""#,
            "check the second ticket covers the disk image",
        ),
        call_line(
            &code,
            r#"xcrun stapler staple "$DMG""#,
            "staple the disk image",
        ),
        call_line(&code, r#"quarantine "$CHECK_DMG""#, "check the disk image"),
        call_line(
            &code,
            r#"cp "$DMG" "$OUT/$APP_DMG""#,
            "write the disk image",
        ),
    ];
    assert!(
        steps.windows(2).all(|w| w[0] < w[1]),
        "{SCRIPT} must run the stages in this order: app and command, staple, disk image, \
         staple, checks, files ({steps:?})"
    );
    // The first upload holds the app and the command, not the disk image.
    let (zip, _) = line_with_all(
        &code,
        &["ditto -c -k --keepParent", r#""$NOTARY.zip""#],
        "zip the first upload",
    );
    for needle in [r#"ditto "$APP" "$NOTARY/"#, r#"cp "$CLI" "$NOTARY/"#] {
        let (i, _) = line_with_all(&code, &[needle], "put the app and command in the upload");
        assert!(i < zip, "{needle:?} must come before the zip");
    }
    assert!(
        !code.iter().any(|l| l.contains(r#""$DMG" "$NOTARY/"#)),
        "the first upload must not hold the disk image"
    );
}

/// A resumed run carries on at the stage it stopped in: once the disk
/// image's submission is saved, the first stage (and the disk image's
/// build) is not repeated, and the image Apple got is found from its record.
#[test]
fn script_resumes_at_the_stage_it_stopped_in() {
    let code = code_lines(&script_text());
    let (guard, _) = line_with_all(
        &code,
        &["if [ ! -f \"$NOTARY_DMG.json\" ]; then"],
        "skip the first stage once the disk image is uploaded",
    );
    let make = call_line(&code, "make_dmg", "build the disk image");
    let upload = call_line(&code, DMG_UPLOAD, "upload the disk image");
    let wait = call_line(&code, r#"wait_for "$NOTARY_DMG""#, "wait for it");
    assert!(
        guard < make && make < upload && upload < wait,
        "the first stage, the build and the second upload must sit inside the guard"
    );
    let (path, _) = line_with_all(
        &code,
        &[r#">"$NOTARY_DMG.path""#],
        "record which disk image goes to Apple",
    );
    assert!(
        make < path && path < upload,
        "{SCRIPT} must record the disk image's path before uploading it"
    );
    line_with_all(
        &code,
        &[r#"DMG=$(cat "$NOTARY_DMG.path")"#],
        "find the uploaded disk image again on --resume",
    );
}

#[test]
fn script_checks_the_ticket_covers_the_signed_items_before_stapling() {
    let text = script_text();
    let check = shell_function(&text, "check_ticket")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_ticket"));
    for needle in ["notarytool log", "CDHash", "grep -qi"] {
        assert!(check.contains(needle), "check_ticket must use {needle:?}");
    }
    let code = code_lines(&text);
    for (ticket, staple) in [
        (
            r#"check_ticket "$NOTARY" "$APP" "$CLI""#,
            r#"xcrun stapler staple "$APP""#,
        ),
        (
            r#"check_ticket "$NOTARY_DMG" "$DMG""#,
            r#"xcrun stapler staple "$DMG""#,
        ),
    ] {
        let t = call_line(&code, ticket, "check the ticket");
        let s = call_line(&code, staple, "staple");
        assert!(t < s, "{SCRIPT} must run {ticket:?} before {staple:?}");
    }
}

/// `--resume` finishes with the channel and label of the interrupted run,
/// so the record is written before anything is sent to Apple.
#[test]
fn script_records_the_channel_before_the_upload() {
    let code = code_lines(&script_text());
    let record = code
        .iter()
        .position(|l| l.contains("release-info") && l.contains('>'))
        .unwrap_or_else(|| panic!("{SCRIPT} must write work/release-info"));
    let upload = call_line(&code, APP_UPLOAD, "upload the app and the command");
    assert!(
        record < upload,
        "{SCRIPT} must record the channel before uploading"
    );
}

/// Full Disk Access survives a new build only while the designated
/// requirement stays the bundle ID plus the team, so the script compares it
/// with the exact expected text before the upload and before any file is
/// written.
#[test]
fn script_checks_the_designated_requirement_before_uploading() {
    let text = script_text();
    assert!(
        text.contains(REQUIREMENT),
        "{SCRIPT} must spell out the designated requirement {REQUIREMENT:?}"
    );
    assert!(
        text.contains(r#"TEAM_ID=$(team_of "$IDENTITY")"#),
        "{SCRIPT} must take the team from the signing identity"
    );
    let code = code_lines(&text);
    let check = first_line_with(&code, "codesign -d -r-")
        .unwrap_or_else(|| panic!("{SCRIPT} must read the requirement with codesign -d -r-"));
    let submit = call_line(&code, APP_UPLOAD, "upload the app and the command");
    let zip = first_line_with(&code, "ditto -c -k --norsrc")
        .unwrap_or_else(|| panic!("{SCRIPT} must zip the app"));
    let function = first_line_with(&code, "check_requirement()");
    let calls: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "check_requirement")
        .map(|(i, _)| i)
        .collect();
    assert!(
        function.is_some_and(|f| f < check),
        "{SCRIPT} must check the requirement in a check_requirement function"
    );
    assert!(
        calls.iter().any(|&c| c < submit),
        "{SCRIPT} must check the requirement before the upload"
    );
    assert!(
        calls.iter().all(|&c| c < zip),
        "{SCRIPT} must check the requirement before writing the release files"
    );
}

/// The release files carry the third-party notices: the script stops when `THIRD-PARTY-NOTICES.md` doesn't match
/// `Cargo.lock` (`check_notices`: `scripts/third-party-notices.sh
/// --check`), before building a new release and before `--resume` goes on,
/// and the CLI tarball holds it next to `LICENSE`; the app carries it
/// inside.
#[test]
fn script_ships_current_third_party_notices() {
    let text = script_text();
    let function = shell_function(&text, "check_notices")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_notices"));
    assert!(
        function.contains("third-party-notices.sh\" --check"),
        "check_notices must run the notices check: {function}"
    );
    let code = code_lines(&text);
    let checks = call_lines(&code, "check_notices");
    assert_eq!(
        checks.len(),
        2,
        "{SCRIPT} must check the notices for a new build and for --resume"
    );
    let cargo = first_line_with(&code, "command -v cargo")
        .unwrap_or_else(|| panic!("{SCRIPT} must check for cargo"));
    let mkdir = first_line_with(&code, "mkdir \"$DIR\"")
        .unwrap_or_else(|| panic!("{SCRIPT} must create the output folder"));
    assert!(
        checks.iter().any(|&c| cargo < c && c < mkdir),
        "{SCRIPT} must check the notices after finding cargo and before building"
    );
    let requirement = *call_lines(&code, "check_requirement")
        .first()
        .unwrap_or_else(|| panic!("{SCRIPT} must check the requirement"));
    assert!(
        checks.iter().any(|&c| c < requirement),
        "{SCRIPT} --resume must check the notices before going on"
    );
    let lines = logical_lines(&code);
    let (_, copy) = lines
        .iter()
        .find(|(_, l)| l.contains("\"$WORK/cli/\"") && l.trim_start().starts_with("cp "))
        .unwrap_or_else(|| panic!("{SCRIPT} must copy the tarball's files"));
    assert!(
        copy.contains("\"$ROOT/THIRD-PARTY-NOTICES.md\""),
        "{SCRIPT} must copy the notices for the tarball: {copy}"
    );
    let (_, tar) = lines
        .iter()
        .find(|(_, l)| l.contains("tar --no-mac-metadata"))
        .unwrap_or_else(|| panic!("{SCRIPT} must make the CLI tarball"));
    assert!(
        words(tar).contains(&"THIRD-PARTY-NOTICES.md".to_owned()),
        "{SCRIPT} must put the notices in the CLI tarball: {tar}"
    );
}

/// The lines of `code` that are exactly `call`.
fn call_lines(code: &[String], call: &str) -> Vec<usize> {
    code.iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == call)
        .map(|(i, _)| i)
        .collect()
}

/// The signing identity comes from the builder's environment, never from
/// the script, so the public repository names no one: `NE_SIGN_IDENTITY`
/// names the
/// Developer ID Application certificate, and the team is the ID in its
/// closing brackets. A new build checks it before looking in the keychain;
/// `--resume` checks it before the designated requirement.
#[test]
fn script_takes_the_signing_identity_from_the_environment() {
    let text = script_text();
    assert!(
        text.contains("IDENTITY=${NE_SIGN_IDENTITY:-}"),
        "{SCRIPT} must read the identity from NE_SIGN_IDENTITY, with no default"
    );
    let code = code_lines(&text);
    let checks = call_lines(&code, "check_identity");
    assert_eq!(
        checks.len(),
        2,
        "{SCRIPT} must check the identity once for a new build and once for --resume"
    );
    let keychain = code
        .iter()
        .position(|l| l.trim().starts_with("security find-identity"))
        .unwrap_or_else(|| panic!("{SCRIPT} must look for the identity in the keychain"));
    assert!(
        checks.iter().any(|&c| c < keychain),
        "{SCRIPT} must check the identity before looking in the keychain"
    );
    let requirement = call_lines(&code, "check_requirement");
    let first = *requirement
        .first()
        .unwrap_or_else(|| panic!("{SCRIPT} must check the requirement"));
    assert!(
        checks.iter().any(|&c| c < first),
        "{SCRIPT} must check the identity before the first requirement check"
    );
}

/// Rust keeps source paths in a binary for its panic messages, the
/// dependencies' under the builder's `~/.cargo`. The release build maps the
/// home folder to `~`, and the script stops before signing if the app or
/// the command still holds it, which would ship the builder's user name.
#[test]
fn script_keeps_the_home_folder_out_of_the_binaries() {
    let text = script_text();
    assert!(
        text.contains(r#"RUSTFLAGS="--remap-path-prefix=$HOME=~""#),
        "{SCRIPT} must map the home folder out of the build's paths"
    );
    let code = code_lines(&text);
    let build = first_line_with(&code, RELEASE_BUILD)
        .unwrap_or_else(|| panic!("{SCRIPT} must build the app and the command"));
    let check = first_line_with(&code, "check_no_home \"$TARGET/release/")
        .unwrap_or_else(|| panic!("{SCRIPT} must check the binaries for the home folder"));
    let sign = first_line_with(&code, "--app --sign-identity")
        .unwrap_or_else(|| panic!("{SCRIPT} must sign the app"));
    assert!(
        build < check && check < sign,
        "{SCRIPT} must check the binaries after building them and before signing"
    );
}

/// tar records each file's owner, extended attributes, ACLs and file flags,
/// so a plain tarball would publish the builder's account name (as owner,
/// or inside an ACL) and the files' `com.apple.provenance`. `make_cli_tar`
/// lists its files as root:wheel with none of those, and the script checks
/// the tarball (`check_tarball`) right after writing it, before summing it.
#[test]
fn script_makes_a_cli_tarball_that_names_no_one() {
    let text = script_text();
    let make = shell_function(&text, "make_cli_tar")
        .unwrap_or_else(|| panic!("{SCRIPT} must define make_cli_tar"));
    for flag in [
        "COPYFILE_DISABLE=1",
        "--no-mac-metadata",
        "--no-xattrs",
        "--no-acls",
        "--no-fflags",
        "--uid 0",
        "--gid 0",
        "--uname root",
        "--gname wheel",
    ] {
        assert!(make.contains(flag), "make_cli_tar must use {flag}: {make}");
    }
    let check = shell_function(&text, "check_tarball")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_tarball"));
    for needle in [
        "tar -tvzf",
        "root:wheel",
        r"(LIBARCHIVE|SCHILY)\.'",
        "pax -z -f",
    ] {
        assert!(
            check.contains(needle),
            "check_tarball must use {needle}: {check}"
        );
    }
    let code = code_lines(&text);
    let made = call_line(
        &code,
        r#"make_cli_tar "$OUT/$CLI_TAR" "$WORK/cli""#,
        "make the CLI tarball",
    );
    let checked = call_line(
        &code,
        r#"check_tarball "$OUT/$CLI_TAR""#,
        "check the CLI tarball",
    );
    let summed = first_line_with(&code, "shasum -a 256")
        .unwrap_or_else(|| panic!("{SCRIPT} must sum the files"));
    assert!(
        made < checked && checked < summed,
        "{SCRIPT} must check the tarball after making it and before summing it"
    );
}

/// The script's per-channel names are the app's and the command's own
/// (`fx_channel`), so the bundle it expects is the one the build writes.
#[test]
fn script_names_match_the_channels() {
    let text = script_text();
    for channel in Channel::ALL {
        for (var, value) in [
            ("APP_NAME", channel.app_name()),
            ("BUNDLE_ID", channel.bundle_id()),
            ("CLI_NAME", channel.command_name()),
        ] {
            let assignment = format!("{var}=\"{value}\"");
            assert!(
                text.contains(&assignment),
                "{SCRIPT} must set {assignment} for {channel:?}"
            );
        }
    }
}

#[test]
fn script_writes_the_release_and_preview_file_names() {
    let text = script_text();
    for name in [
        "APP_ZIP=Not-Everything-$LABEL-macos-arm64.zip",
        "CLI_TAR=not-everything-cli-$LABEL-macos-arm64.tar.gz",
        "APP_ZIP=Not-Everything-Preview-$LABEL-macos-arm64.zip",
        "CLI_TAR=not-everything-preview-cli-$LABEL-macos-arm64.tar.gz",
        "SHA256SUMS.txt",
    ] {
        assert!(text.contains(name), "{SCRIPT} must write {name}");
    }
}

// -- The disk image. ---------------------------------------------------------

/// The first command (continuation lines joined) that contains every
/// needle, with the index of its first line in `code`, or a panic naming
/// `what`.
fn line_with_all(code: &[String], needles: &[&str], what: &str) -> (usize, String) {
    logical_lines(code)
        .into_iter()
        .find(|(_, l)| needles.iter().all(|n| l.contains(n)))
        .unwrap_or_else(|| panic!("{SCRIPT} must {what} (a command with {needles:?})"))
}

/// Release `Not-Everything-<version>-macos-arm64.dmg` (volume
/// "Not-Everything <version>") and Preview
/// `Not-Everything-Preview-<label>-macos-arm64.dmg` (volume "Not-Everything
/// Preview <label>"), summed in `SHA256SUMS.txt` with the zip and the
/// tarball.
#[test]
fn script_names_the_disk_images_and_sums_them() {
    let text = script_text();
    for name in [
        "APP_DMG=Not-Everything-$LABEL-macos-arm64.dmg",
        "APP_DMG=Not-Everything-Preview-$LABEL-macos-arm64.dmg",
        r#"VOLUME="$APP_NAME $LABEL""#,
    ] {
        assert!(text.contains(name), "{SCRIPT} must set {name}");
    }
    let code = code_lines(&text);
    line_with_all(
        &code,
        &["hdiutil create", r#"-volname "$VOLUME""#],
        "name the disk image's volume",
    );
    line_with_all(
        &code,
        &["shasum -a 256", "$APP_ZIP", "$APP_DMG", "$CLI_TAR"],
        "sum the zip, the disk image and the tarball",
    );
    line_with_all(
        &code,
        &[r#"cp "$DMG" "$OUT/$APP_DMG""#],
        "write the disk image next to the zip",
    );
}

/// The disk images are signed with each channel's bundle ID plus `.dmg`,
/// and those are exactly the identifiers the owner approved as exceptions
/// to the rule that no other name contains "Everything".
#[test]
fn disk_image_identifiers_are_the_approved_names() {
    const APPROVED: [&str; 2] = [
        "io.3645.not-everything.dmg",
        "io.3645.not-everything.preview.dmg",
    ];
    let code = code_lines(&script_text());
    line_with_all(
        &code,
        &["codesign", r#"--identifier "$BUNDLE_ID.dmg""#, r#""$DMG""#],
        "sign the disk image as the bundle ID plus .dmg",
    );
    let mut ids: Vec<String> = Channel::ALL
        .iter()
        .map(|c| format!("{}.dmg", c.bundle_id()))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids, APPROVED,
        "the disk image identifiers must be the approved ones"
    );
}

/// Apple's packaging guide: populate a folder, make a UDIF read-only
/// zlib-compressed image (UDZO) from it, sign it with the Developer ID, a
/// secure timestamp and its own identifier, and verify it before
/// notarizing. `make_dmg` copies `$APP`, which a release or an RC has
/// stapled by then; each build gets a fresh folder, so a build cut short is
/// never reused or overwritten.
#[test]
fn script_makes_and_signs_the_disk_image_from_the_app() {
    let text = script_text();
    let make = shell_function(&text, "make_dmg")
        .unwrap_or_else(|| panic!("{SCRIPT} must define make_dmg"));
    let code = code_lines(&make);
    let steps = [
        line_with_all(
            &code,
            &["mktemp -d", "$WORK/dmg."],
            "build in a fresh folder",
        ),
        line_with_all(&code, &["ditto", r#""$APP""#], "copy the app in"),
        line_with_all(&code, &["ln -s /Applications"], "link to /Applications"),
        line_with_all(
            &code,
            &["hdiutil create", "-srcfolder", "-format UDRW"],
            "make a read-write image to lay out",
        ),
        line_with_all(
            &code,
            &["hdiutil convert", "-format UDZO", r#""$DMG""#],
            "convert it to a read-only UDZO image",
        ),
        line_with_all(
            &code,
            &[
                "codesign",
                r#"--sign "$IDENTITY""#,
                "--timestamp",
                r#"--identifier "$BUNDLE_ID.dmg""#,
                r#""$DMG""#,
            ],
            "sign the image with the Developer ID",
        ),
        line_with_all(
            &code,
            &["hdiutil verify", r#""$DMG""#],
            "verify the image before uploading it",
        ),
    ];
    assert!(
        steps.windows(2).all(|w| w[0].0 < w[1].0),
        "make_dmg must run its steps in order: {steps:?}"
    );
    // A beta (not notarized) builds it right after signing.
    let code = code_lines(&text);
    let calls: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "make_dmg")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        calls.len(),
        2,
        "one make_dmg call per stage flow and for betas"
    );
    let staple = call_line(&code, r#"xcrun stapler staple "$APP""#, "staple the app");
    assert!(
        calls[0] > staple,
        "a notarized build must staple the app first"
    );
}

/// Finder lays out the window (fixed size, icon positions) on a read-write
/// image; if Finder can't be scripted or macOS denies it, the image keeps
/// the plain layout and the script says so instead of stopping.
#[test]
fn script_lays_out_the_window_with_finder_or_says_it_could_not() {
    let text = script_text();
    for needle in [
        r#"tell application "Finder""#,
        "set bounds of theWindow to",
        "set arrangement of viewOptions to not arranged",
        "set icon size of viewOptions to",
        "set position of item appName",
        r#"set position of item "Applications""#,
        "plain layout",
    ] {
        assert!(text.contains(needle), "{SCRIPT} must use {needle:?}");
    }
    let code = code_lines(&text);
    let (_, osascript) = line_with_all(&code, &["osascript"], "run Finder's script");
    assert!(
        osascript.trim_start().starts_with("if osascript"),
        "a failed Finder script must not stop the release: {osascript:?}"
    );
    line_with_all(
        &code,
        &["attach_image", "-readwrite"],
        "attach the layout image read-write",
    );
}

/// A quarantined copy of the disk image, as a download arrives, passes
/// `hdiutil verify`, Gatekeeper's check for opening it (a release or an RC:
/// "Notarized Developer ID") and `stapler validate`, and the app inside it
/// passes the app checks, all before the release files are written.
#[test]
fn script_checks_a_quarantined_disk_image_before_writing_the_files() {
    let code = code_lines(&script_text());
    let (write, _) = line_with_all(
        &code,
        &[r#"cp "$DMG" "$OUT/$APP_DMG""#],
        "write the disk image",
    );
    let zip = first_line_with(&code, "ditto -c -k --norsrc").unwrap();
    for (needles, what) in [
        (&[r#"quarantine "$CHECK_DMG""#][..], "quarantine the copy"),
        (
            &["hdiutil verify", r#""$CHECK_DMG""#],
            "verify the quarantined copy",
        ),
        (
            &[
                "spctl --assess --type open --context context:primary-signature",
                r#""$CHECK_DMG""#,
            ],
            "ask Gatekeeper about opening the disk image",
        ),
        (
            &[r#"stapler validate "$CHECK_DMG""#],
            "validate the copy's stapled ticket",
        ),
        (
            &["attach_image", r#""$CHECK_DMG""#, "-readonly"],
            "mount the copy read-only",
        ),
        (
            &[r#"check_app "$MOUNTED/$APP_NAME.app""#],
            "check the app inside the disk image",
        ),
    ] {
        let (i, line) = line_with_all(&code, needles, what);
        assert!(
            i < write && i < zip,
            "{SCRIPT} must run {line:?} before writing the files"
        );
    }
    let notarized = code
        .iter()
        .filter(|l| l.contains("source=Notarized Developer ID"))
        .count();
    assert!(
        notarized >= 2,
        "{SCRIPT} must require \"Notarized Developer ID\" for the disk image and the app"
    );
    // The check that caught RC-1's unstapled app inside the disk image.
    let check_app = shell_function(&script_text(), "check_app")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_app"));
    assert!(
        check_app.contains(r#"syspolicy_check distribution "$1""#),
        "check_app must run syspolicy_check distribution"
    );
}

/// Every image goes through `attach_image`, and `detach_image` runs on exit,
/// so a failed check never leaves an image mounted.
#[test]
fn script_detaches_the_images_it_attaches() {
    let text = script_text();
    let code = code_lines(&text);
    let attaches = code.iter().filter(|l| l.contains("hdiutil attach")).count();
    assert_eq!(
        attaches, 1,
        "{SCRIPT} must attach images only in attach_image"
    );
    let attach = shell_function(&text, "attach_image")
        .unwrap_or_else(|| panic!("{SCRIPT} must define attach_image"));
    assert!(attach.contains("hdiutil attach") && attach.contains("MOUNTED="));
    let detach = shell_function(&text, "detach_image")
        .unwrap_or_else(|| panic!("{SCRIPT} must define detach_image"));
    assert!(detach.contains("hdiutil detach"));
    assert!(
        code.iter().any(|l| l.trim() == "trap detach_image EXIT"),
        "{SCRIPT} must detach on exit"
    );
}

/// Release candidates and releases upload the disk image to their GitHub
/// page with the other files.
#[test]
fn script_publishes_the_disk_image() {
    let code = code_lines(&script_text());
    let creates: Vec<String> = logical_lines(&code)
        .into_iter()
        .map(|(_, l)| l)
        .filter(|l| l.contains("gh release create"))
        .collect();
    assert_eq!(creates.len(), 2, "one gh release create per RC and release");
    for line in creates {
        for file in [
            r#""$OUT/$APP_ZIP""#,
            r#""$OUT/$APP_DMG""#,
            r#""$OUT/$CLI_TAR""#,
            r#""$OUT/SHA256SUMS.txt""#,
        ] {
            assert!(line.contains(file), "{line:?} must upload {file}");
        }
    }
}

// -- Pure helpers. -----------------------------------------------------------

#[test]
fn shell_function_returns_a_whole_top_level_function() {
    let script = "a() { echo a; }\n\
                  b() {\n    if x; then\n        y\n    fi\n}\n\
                  \x20   c() {\n        z\n    }\n";
    assert_eq!(
        shell_function(script, "a").as_deref(),
        Some("a() { echo a; }")
    );
    assert_eq!(
        shell_function(script, "b").as_deref(),
        Some("b() {\n    if x; then\n        y\n    fi\n}")
    );
    assert_eq!(
        shell_function(script, "c"),
        None,
        "only top-level functions"
    );
    assert_eq!(shell_function(script, "d"), None);
    assert_eq!(
        shell_function("e() {\n  open\n", "e"),
        None,
        "no closing brace"
    );
}

#[test]
fn logical_lines_join_continuations() {
    let code: Vec<String> = ["a \\", "    b \\", "    c", "d", "e\\", "f"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        logical_lines(&code),
        vec![
            (0, "a b c".to_owned()),
            (3, "d".to_owned()),
            (4, "e f".to_owned()),
        ]
    );
}

#[test]
fn script_builds_with_the_channel_variables() {
    let code = code_lines(&script_text()).join("\n");
    for needle in [
        "NE_CHANNEL=$CHANNEL",
        "NE_CHANNEL_NUMBER=$NUMBER",
        "unset NE_CHANNEL_NUMBER",
        "export NE_CHANNEL",
    ] {
        assert!(code.contains(needle), "{SCRIPT} must use {needle:?}");
    }
}

/// The release build uses exactly the dependencies in `Cargo.lock`, the
/// ones the third-party notices list.
#[test]
fn script_builds_from_the_locked_dependencies() {
    let code = code_lines(&script_text());
    let (_, line) = line_with_all(
        &code,
        &["cargo build --release"],
        "build the app and the command in release",
    );
    assert!(
        words(&line).contains(&"--locked".to_owned()),
        "{SCRIPT} must build with --locked: {line}"
    );
}

/// A release or an RC checks the notary profile before anything is built,
/// so a missing or broken profile never costs a build and a signing; a beta
/// is not notarized and needs none. `--resume` checks it before going on.
#[test]
fn script_checks_the_notary_profile_before_building() {
    let text = script_text();
    let function = shell_function(&text, "check_notary_profile")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_notary_profile"));
    assert!(
        function.contains(r#"xcrun notarytool history --keychain-profile "$PROFILE""#),
        "check_notary_profile must ask Apple for the profile's history: {function}"
    );
    let code = code_lines(&text);
    let calls: Vec<(usize, &String)> = code
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("check_notary_profile") && !l.contains("()"))
        .collect();
    assert_eq!(
        calls.len(),
        2,
        "{SCRIPT} must check the profile for a new build and for --resume: {calls:?}"
    );
    let cargo = first_line_with(&code, "command -v cargo").unwrap();
    let mkdir = first_line_with(&code, "mkdir \"$DIR\"").unwrap();
    let new_build = calls
        .iter()
        .find(|(i, _)| cargo < *i && *i < mkdir)
        .unwrap_or_else(|| {
            panic!("{SCRIPT} must check the profile after finding cargo and before building")
        });
    assert!(
        new_build
            .1
            .contains(r#"[ "$CHANNEL" = beta ] || check_notary_profile"#),
        "a beta needs no notary profile: {}",
        new_build.1
    );
    let resume = first_line_with(&code, "step \"Resume").unwrap();
    assert!(
        calls.iter().any(|(i, _)| *i < resume && *i < cargo),
        "{SCRIPT} --resume must check the profile before going on"
    );
}

/// `--publish` tags and publishes only in the project's repository: the
/// Plan prints `origin`, a new build refuses another origin before fetching
/// from it, and `check_publish_tools` (new builds and `--resume`) checks
/// origin and, unless it is a beta, that `gh` is signed in and points at
/// the same repository. `NE_RELEASE_REPO` names another one, for testing.
#[test]
fn script_publishes_only_to_the_project_repository() {
    let text = script_text();
    assert!(
        text.contains("RELEASE_REPO=${NE_RELEASE_REPO:-3645-io/Not-Everything}"),
        "{SCRIPT} must publish to 3645-io/Not-Everything unless NE_RELEASE_REPO says otherwise"
    );
    let header: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        header.contains("NE_RELEASE_REPO"),
        "{SCRIPT} must document NE_RELEASE_REPO"
    );
    let tools = shell_function(&text, "check_publish_tools")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_publish_tools"));
    for needle in [
        "check_origin",
        "gh auth status",
        "gh repo view --json nameWithOwner",
    ] {
        assert!(
            tools.contains(needle),
            "check_publish_tools must use {needle:?}: {tools}"
        );
    }
    let code = code_lines(&text);
    let plan = first_line_with(&code, "step \"Plan\"").unwrap();
    let origin = first_line_with(&code, "echo \"Origin:")
        .unwrap_or_else(|| panic!("{SCRIPT} must print origin in its Plan"));
    assert!(plan < origin, "the Plan step must print origin");
    let early = code
        .iter()
        .position(|l| l.contains(r#"[ "$PUBLISH" -eq 0 ] || check_origin"#))
        .unwrap_or_else(|| panic!("{SCRIPT} must check origin early with --publish"));
    let fetch = first_line_with(&code, "fetch --quiet --tags origin").unwrap();
    assert!(
        early < fetch,
        "{SCRIPT} must refuse a wrong origin before fetching from it"
    );
}

/// Only gh's "release not found" means there is no release: any other
/// error (signed out, no network, the wrong repository) stops before
/// anything is tagged.
#[test]
fn script_takes_only_release_not_found_as_no_release() {
    let text = script_text();
    let function = shell_function(&text, "check_no_release")
        .unwrap_or_else(|| panic!("{SCRIPT} must define check_no_release"));
    assert!(function.contains("gh release view") && function.contains("release not found"));
    let code = code_lines(&text);
    let check = code
        .iter()
        .position(|l| l.contains(r#"check_no_release "$TAG""#))
        .unwrap_or_else(|| panic!("{SCRIPT} must check for an existing release"));
    let tag = first_line_with(&code, "publish_step git tag -a").unwrap();
    assert!(check < tag, "{SCRIPT} must check before tagging");
    let views = code
        .iter()
        .filter(|l| strip_quoted(l).contains("gh release view"))
        .count();
    assert_eq!(views, 1, "{SCRIPT} must ask gh only in check_no_release");
}

/// Tags and GitHub pages only with `--publish`: every command that tags,
/// pushes or creates a release goes through `publish_step`, which prints it
/// instead of running it unless `--publish` was given.
#[test]
fn script_tags_and_publishes_only_with_publish() {
    let text = script_text();
    let code = code_lines(&text);
    let mut seen = 0;
    for line in &code {
        let bare = strip_quoted(line);
        let w = words(&bare);
        let has = |s: &str| w.iter().any(|x| x == s);
        let tags = has("git") && has("tag") && has("-a");
        let pushes = has("git") && has("push");
        let creates = w
            .windows(3)
            .any(|p| p[0] == "gh" && p[1] == "release" && p[2] == "create");
        if tags || pushes || creates {
            seen += 1;
            assert!(
                line.trim_start().starts_with("publish_step "),
                "{SCRIPT} must tag, push and publish only through publish_step: {line:?}"
            );
        }
    }
    assert!(
        seen >= 3,
        "{SCRIPT} must tag, push the tag and create releases"
    );
    let function = code
        .iter()
        .position(|l| l.contains("publish_step()"))
        .unwrap_or_else(|| panic!("{SCRIPT} must define publish_step"));
    assert!(
        code[function..]
            .iter()
            .take(4)
            .any(|l| l.contains("\"$PUBLISH\" -eq 1")),
        "publish_step must run commands only with --publish"
    );
    let joined = code.join("\n");
    assert!(joined.contains("--prerelease"), "RCs are pre-releases");
    assert!(
        joined.contains("--latest"),
        "releases are the latest release"
    );
    assert!(
        joined.contains("--verify-tag"),
        "gh must use the pushed tag"
    );
    assert!(
        joined.contains("CHANGELOG.md"),
        "release notes come from CHANGELOG.md"
    );
}

#[test]
fn script_documents_its_modes() {
    let text = script_text();
    for usage in [
        "scripts/release-macos.sh [--rc | --beta] [--perf PERF_DIR] [--targets-only] [--publish] NEW_OUT_DIR",
        "scripts/release-macos.sh --resume OUT_DIR [--perf PERF_DIR] [--targets-only] [--publish]",
    ] {
        assert!(text.contains(usage), "{SCRIPT} usage must show {usage:?}");
    }
    let code = code_lines(&text).join("\n");
    for option in [
        "--resume",
        "--rc",
        "--beta",
        "--publish",
        "--perf",
        "--targets-only",
    ] {
        assert!(
            code.contains(option),
            "{SCRIPT} must handle the `{option}` option"
        );
    }
    let header: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "--perf PERF_DIR",
        "perf-check.sh --tier rc",
        "docs/gates/releases/",
        "--targets-only",
    ] {
        assert!(
            header.contains(needle),
            "{SCRIPT}'s header must describe --perf ({needle:?})"
        );
    }
}

// -- The performance table. ---------------------------------------------------

/// The build of the app and the command, which also builds fx-bench.
const RELEASE_BUILD: &str = "build -p fx-ui-macos -p fx-cli -p fx-bench --bins";

/// The performance table is made with the fx-bench of the release's own
/// build (a fresh target folder), built in the same cargo build as the app
/// and the command rather than a second time. It is not shipped, so it is
/// neither checked for the home folder nor packaged or published.
#[test]
fn script_builds_fx_bench_with_the_release_but_does_not_ship_it() {
    let code = code_lines(&script_text());
    let builds: Vec<&str> = code
        .iter()
        .map(|l| l.trim())
        .filter(|l| l.starts_with("build -p"))
        .collect();
    assert_eq!(
        builds,
        [RELEASE_BUILD, "build -p fx-ui-macos --example make_bundle"],
        "{SCRIPT} must build fx-bench with the app and the command, once"
    );
    for (_, line) in logical_lines(&code) {
        let ships = [
            "check_no_home \"",
            "make_bundle\"",
            "make_cli_tar \"",
            "shasum -a 256",
            "gh release create",
            "\"$WORK/cli/\"",
        ]
        .iter()
        .any(|n| line.contains(n));
        assert!(
            !(ships && line.contains("fx-bench")),
            "{SCRIPT} must not ship fx-bench: {line}"
        );
    }
}

/// With `--perf`, once the disk image is final (stapled and its quarantined
/// copy checked) and before the files are written, so a failure here can
/// still be resumed, `perf_section` writes `$OUT/perf-metrics.txt` (PERF_DIR's
/// `metrics.txt` and the disk image's size as `dmg_bytes`), has the release
/// build's fx-bench compare it with the baseline and the previous release
/// (`perf-release`, with PERF_DIR's `notes.md` when there is one) into
/// `$OUT/performance.md`, and keeps the values as `metrics-<label>.txt`.
#[test]
fn script_makes_the_performance_table_once_the_disk_image_is_final() {
    let text = script_text();
    let section = shell_function(&text, "perf_section")
        .unwrap_or_else(|| panic!("{SCRIPT} must define perf_section"));
    let code = code_lines(&section);
    line_with_all(
        &code,
        &["stat -f %z", r#""$DMG""#],
        "read the disk image's size",
    );
    line_with_all(&code, &["dmg_bytes="], "write dmg_bytes");
    line_with_all(
        &code,
        &[r#""$PERF_DIR/metrics.txt""#],
        "copy PERF_DIR's metrics",
    );
    line_with_all(
        &code,
        &[r#">"$OUT/perf-metrics.txt""#],
        "write $OUT/perf-metrics.txt",
    );
    let (_, call) = line_with_all(
        &code,
        &[r#""$TARGET/release/fx-bench" perf-release"#],
        "run the release build's fx-bench perf-release",
    );
    for arg in [
        r#"--baseline "$ROOT/docs/gates/perf-baseline.toml""#,
        r#"--previous "$ROOT/docs/gates/releases/$PREV.txt""#,
        r#"--previous-label "$PREV""#,
        r#"--label "$LABEL""#,
        r#""$OUT/perf-metrics.txt""#,
        r#">"$OUT/performance.md""#,
    ] {
        assert!(call.contains(arg), "perf-release must get {arg}: {call}");
    }
    assert!(
        call.trim_end().ends_with("||"),
        "a failing perf-release must stop the release: {call}"
    );
    line_with_all(
        &code,
        &[r#"--notes "$PERF_DIR/notes.md""#],
        "pass the owner's notes",
    );
    line_with_all(
        &code,
        &[r#"cp "$OUT/perf-metrics.txt" "$OUT/metrics-$LABEL.txt""#],
        "keep the values as metrics-<label>.txt",
    );

    let code = code_lines(&text);
    let section = call_line(&code, "perf_section", "make the performance table");
    let guard = code[..section]
        .iter()
        .rposition(|l| l.trim() == r#"if [ -n "$PERF_DIR" ]; then"#)
        .unwrap_or_else(|| panic!("{SCRIPT} must make the table only with --perf"));
    let staple = call_line(
        &code,
        r#"xcrun stapler staple "$DMG""#,
        "staple the disk image",
    );
    let (checked, _) = line_with_all(
        &code,
        &[r#"check_app "$MOUNTED/$APP_NAME.app""#],
        "check the app in the disk image",
    );
    let write = first_line_with(&code, "ditto -c -k --norsrc").unwrap();
    let notes = call_line(
        &code,
        r#"[ "$CHANNEL" = beta ] || release_notes >"$NOTES""#,
        "write the release notes",
    );
    assert!(
        staple < checked && checked < guard && guard < section && section < write,
        "{SCRIPT} must make the table after the disk image is stapled and checked, \
         and before writing the files"
    );
    assert!(section < notes, "the table must exist before the notes");
    let done = first_line_with(&code, "step \"Done").unwrap();
    assert!(
        code[done..]
            .iter()
            .any(|l| l.contains("echo") && l.contains("docs/gates/releases/$VERSION.txt")),
        "the Done output must say metrics-<label>.txt becomes docs/gates/releases/<X.Y.Z>.txt"
    );
}

/// The release notes are written by `release_notes`, which puts the table
/// after the CHANGELOG.md entry; the behaviour test runs it.
#[test]
fn script_puts_the_table_after_the_changelog_entry() {
    let text = script_text();
    let notes = shell_function(&text, "release_notes")
        .unwrap_or_else(|| panic!("{SCRIPT} must define release_notes"));
    let code = code_lines(&notes);
    let entry = code
        .iter()
        .rposition(|l| l.trim_start().starts_with("changelog_entry"))
        .unwrap_or_else(|| panic!("release_notes must print the CHANGELOG.md entry"));
    let table = first_line_with(&code, r#"cat "$OUT/performance.md""#)
        .unwrap_or_else(|| panic!("release_notes must print the performance table"));
    assert!(entry < table, "the table must come after the entry");
}

/// The lines of `code` that call the function `name` (not its definition).
fn calls_of(code: &[String], name: &str) -> Vec<usize> {
    let head = format!("{name}()");
    code.iter()
        .enumerate()
        .filter(|(_, l)| !l.contains(&head) && words(&strip_quoted(l)).iter().any(|w| w == name))
        .map(|(i, _)| i)
        .collect()
}

/// `--perf` is checked before anything is fetched or built: a new build
/// checks that `--publish` of a release or an RC has it, and the run's
/// files, tier, commit and verdict, before fetching the tags, and the
/// previous release's values after fetching them and before building;
/// `--resume` checks them all before going on.
#[test]
fn script_checks_the_performance_run_before_fetching_or_building() {
    let code = code_lines(&script_text());
    let resume = first_line_with(&code, "step \"Resume").unwrap();
    let fetch = first_line_with(&code, "fetch --quiet --tags origin").unwrap();
    let cargo = first_line_with(&code, "command -v cargo").unwrap();
    for (name, after, before) in [
        ("need_perf", resume, fetch),
        ("check_perf", resume, fetch),
        ("check_previous_release", fetch, cargo),
    ] {
        let calls = calls_of(&code, name);
        assert_eq!(
            calls.len(),
            2,
            "{SCRIPT} must call {name} for --resume and for a new build: {calls:?}"
        );
        assert!(
            calls[0] < resume,
            "--resume must call {name} before going on"
        );
        assert!(
            after < calls[1] && calls[1] < before,
            "a new build must call {name} between lines {after} and {before}: {calls:?}"
        );
    }
    let (_, check) = line_with_all(&code, &["check_perf", "rev-parse HEAD"], "check HEAD");
    assert!(
        check.contains(r#"[ -z "$PERF_DIR" ] ||"#),
        "only --perf is checked: {check}"
    );
    line_with_all(
        &code,
        &["check_perf \"$COMMIT\""],
        "check the build's recorded commit on --resume",
    );
}

// ---------------------------------------------------------------------------
// Script behaviour (refusals only; never builds, signs or contacts Apple).
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod behaviour {
    use super::*;
    use common::shell::{Sandbox, assert_refused, describe, run_with_timeout, temp_path, tree};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    /// Runs the script's `functions` (with `fail`) and then `body` in `sh`,
    /// with `env` set and NE_SIGN_IDENTITY cleared unless `env` sets it.
    fn run_functions(functions: &[&str], body: &str, env: &[(&str, &str)]) -> Output {
        let text = script_text();
        let mut code = String::new();
        for name in ["fail"].iter().chain(functions) {
            let function = shell_function(&text, name)
                .unwrap_or_else(|| panic!("{SCRIPT} must define {name}"));
            code.push_str(&function);
            code.push('\n');
        }
        code.push_str(body);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(code)
            .env_remove("NE_SIGN_IDENTITY")
            .env_remove("NE_RELEASE_REPO");
        for (k, v) in env {
            cmd.env(k, v);
        }
        run_with_timeout(&mut cmd)
    }

    /// `check_identity` refuses a missing identity and a name without a
    /// team, and takes the team from a Developer ID certificate's name.
    #[test]
    fn check_identity_takes_the_team_from_the_certificate_name() {
        let body = "IDENTITY=${NE_SIGN_IDENTITY:-}\ncheck_identity\necho \"team=$TEAM_ID\"";
        let functions = ["team_of", "check_identity"];
        let out = run_functions(&functions, body, &[]);
        assert_refused(&out, 1, "set NE_SIGN_IDENTITY", "no identity");
        let out = run_functions(&functions, body, &[("NE_SIGN_IDENTITY", "Jo Doe")]);
        assert_refused(&out, 1, "team ID in brackets", "a name without a team");
        let name = "Developer ID Application: Jo Doe (AB12CD34EF)";
        let out = run_functions(&functions, body, &[("NE_SIGN_IDENTITY", name)]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("team=AB12CD34EF"),
            "{}",
            describe(&out)
        );
    }

    /// `check_no_home` stops at a binary holding the builder's home folder
    /// and passes one that doesn't.
    #[test]
    fn check_no_home_finds_the_home_folder_in_a_binary() {
        let dir = temp_path("no-home");
        fs::create_dir_all(&dir).unwrap();
        let clean = dir.join("clean");
        let dirty = dir.join("dirty");
        fs::write(&clean, b"\0\x01~/.cargo/registry/src/x.rs\0").unwrap();
        fs::write(&dirty, b"\0\x01/Users/jo/.cargo/registry/src/x.rs\0").unwrap();
        let home = [("HOME", "/Users/jo")];
        let run = |f: &Path| {
            run_functions(
                &["check_no_home"],
                &format!("check_no_home '{}'", f.display()),
                &home,
            )
        };
        let out = run(&clean);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert_refused(
            &run(&dirty),
            1,
            "holds the home folder",
            "a binary with the home folder",
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// The output of `cmd` with `args`, asserting it worked.
    #[cfg(target_os = "macos")]
    fn output(cmd: &str, args: &[&str]) -> Vec<u8> {
        let out = run_with_timeout(Command::new(cmd).args(args));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{cmd} {args:?}\n{}",
            describe(&out)
        );
        out.stdout
    }

    /// `make_cli_tar` lists every file as root:wheel and leaves out their
    /// extended attributes, ACLs and file flags, and `check_tarball` passes
    /// it. `check_tarball` stops at a tarball with another owner, one with
    /// pax headers for extended attributes, for an ACL or for file flags,
    /// and one with AppleDouble `._` files (tar without COPYFILE_DISABLE),
    /// which `tar -t` doesn't list.
    #[cfg(target_os = "macos")]
    #[test]
    fn cli_tarball_names_no_one_and_holds_no_extended_attributes() {
        let dir = temp_path("cli-tar");
        let cli = dir.join("cli");
        fs::create_dir_all(&cli).unwrap();
        let cli = cli.display().to_string();
        let files = [
            "not-everything",
            "cli.md",
            "LICENSE",
            "THIRD-PARTY-NOTICES.md",
        ];
        for name in files {
            let file = format!("{cli}/{name}");
            fs::write(&file, name).unwrap();
            // An extended attribute, as macOS adds com.apple.provenance, an
            // ACL entry (one can name an account) and a file flag.
            output("xattr", &["-w", "io.3645.test", "x", &file]);
            output("chmod", &["+a", "everyone allow read", &file]);
            output("chflags", &["hidden", &file]);
        }
        let run = |functions: &[&str], body: &str| run_functions(functions, body, &[]);

        let made = dir.join("made.tar.gz").display().to_string();
        let body = format!(
            "CLI_NAME=not-everything\nmake_cli_tar '{made}' '{cli}'\ncheck_tarball '{made}'"
        );
        let out = run(&["make_cli_tar", "check_tarball"], &body);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        let list = String::from_utf8(output("tar", &["-tvzf", &made])).unwrap();
        let owners: Vec<(&str, &str, &str)> = list
            .lines()
            .map(|l| {
                let w: Vec<&str> = l.split_whitespace().collect();
                (w[2], w[3], w[w.len() - 1])
            })
            .collect();
        let expected: Vec<_> = files.iter().map(|f| ("root", "wheel", *f)).collect();
        assert_eq!(owners, expected, "{list}");
        let raw = output("gzip", &["-dc", &made]);
        for header in [&b"io.3645.test"[..], b"SCHILY.", b"LIBARCHIVE."] {
            assert!(
                !raw.windows(header.len()).any(|w| w == header),
                "the tarball must hold no {:?} header",
                String::from_utf8_lossy(header)
            );
        }

        // (what, tar's flags, the member, AppleDouble files, the refusal).
        let root = "--uid 0 --gid 0 --uname root --gname wheel";
        for (i, (what, flags, apple_double, phrase)) in [
            (
                "another owner",
                "--no-xattrs --no-acls --no-fflags --uid 501 --gid 20 --uname me --gname staff"
                    .to_owned(),
                false,
                "root:wheel",
            ),
            (
                "pax xattr headers",
                format!("--xattrs --no-acls --no-fflags {root}"),
                false,
                "extended headers",
            ),
            (
                "a pax ACL header",
                format!("--acls --no-xattrs --no-fflags {root}"),
                false,
                "extended headers",
            ),
            (
                "a pax file-flags header",
                format!("--fflags --no-xattrs --no-acls {root}"),
                false,
                "extended headers",
            ),
            (
                "AppleDouble files",
                format!("--no-xattrs --no-acls --no-fflags {root}"),
                true,
                "AppleDouble",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let tarball = dir.join(format!("{i}.tar.gz")).display().to_string();
            let mut tar = Command::new("tar");
            tar.args(flags.split_whitespace())
                .args(["-czf", &tarball, "-C", &cli, "LICENSE"]);
            if apple_double {
                tar.env_remove("COPYFILE_DISABLE");
            } else {
                tar.env("COPYFILE_DISABLE", "1");
            }
            let out = run_with_timeout(&mut tar);
            assert_eq!(out.status.code(), Some(0), "{what}\n{}", describe(&out));
            let out = run(&["check_tarball"], &format!("check_tarball '{tarball}'"));
            assert_refused(&out, 1, phrase, what);
        }
        fs::remove_dir_all(&dir).ok();
    }

    /// A fresh folder holding executable fake commands, as (name, script
    /// body after the shebang), to put first on `PATH`.
    struct FakeTools {
        dir: PathBuf,
    }

    impl FakeTools {
        fn new(tag: &str, tools: &[(&str, &str)]) -> FakeTools {
            use std::os::unix::fs::PermissionsExt;
            let dir = temp_path(tag);
            fs::create_dir_all(&dir).unwrap();
            for (name, body) in tools {
                let path = dir.join(name);
                fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            FakeTools { dir }
        }

        /// `PATH` with the fakes first, then the system folders.
        fn path(&self) -> String {
            format!("{}:{}", self.dir.display(), common::shell::SYSTEM_PATH)
        }
    }

    impl Drop for FakeTools {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// A fake `gh`: `auth status` succeeds unless FAKE_GH_AUTH=out, `repo
    /// view` prints FAKE_GH_REPO (and fails without it), and `release view`
    /// finds no release, finds one (FAKE_GH_RELEASE=exists) or fails as a
    /// signed-out gh does (FAKE_GH_RELEASE=error).
    const FAKE_GH: &str = r#"case "$1 $2" in
"auth status")
    [ "${FAKE_GH_AUTH:-in}" = in ] || { echo "You are not logged into any GitHub hosts." >&2; exit 1; } ;;
"repo view")
    [ -n "${FAKE_GH_REPO:-}" ] || { echo "none of the git remotes point to a known GitHub host" >&2; exit 1; }
    echo "$FAKE_GH_REPO" ;;
"release view")
    case ${FAKE_GH_RELEASE:-missing} in
    missing) echo "release not found" >&2; exit 1 ;;
    exists) echo "title: x"; exit 0 ;;
    *) echo "HTTP 401: Bad credentials (https://api.github.com/graphql)" >&2; exit 1 ;;
    esac ;;
*) echo "unexpected gh $*" >&2; exit 3 ;;
esac"#;

    /// A throwaway repository whose origin is `url` (no network: only
    /// `git remote get-url` reads it), or none.
    fn repo_with_origin(tag: &str, url: Option<&str>) -> Sandbox {
        let repo = Sandbox::new(tag);
        if let Some(url) = url {
            repo.git(&["remote", "add", "origin", url]);
        }
        repo
    }

    /// The script's origin check, run on `repo` with `env`.
    fn check_origin(repo: &Sandbox, env: &[(&str, &str)]) -> Output {
        let body = format!(
            "ROOT='{}'\nRELEASE_REPO=${{NE_RELEASE_REPO:-3645-io/Not-Everything}}\ncheck_origin\necho origin ok",
            repo.dir.display()
        );
        run_functions(&["lower", "github_repo", "check_origin"], &body, env)
    }

    #[test]
    fn check_origin_accepts_only_the_project_repository() {
        for url in [
            "https://github.com/3645-io/Not-Everything",
            "https://github.com/3645-io/Not-Everything.git",
            "https://github.com/3645-IO/not-everything.git",
            "git@github.com:3645-io/Not-Everything.git",
            "ssh://git@github.com/3645-io/Not-Everything.git",
        ] {
            let repo = repo_with_origin("origin-ok", Some(url));
            let out = check_origin(&repo, &[]);
            assert_eq!(out.status.code(), Some(0), "{url}\n{}", describe(&out));
        }
        for url in [
            "https://github.com/someone/Not-Everything.git",
            "https://github.com/3645-io/Not-Everything-old.git",
            "https://gitlab.com/3645-io/Not-Everything.git",
            "/tmp/Not-Everything.git",
        ] {
            let repo = repo_with_origin("origin-wrong", Some(url));
            assert_refused(
                &check_origin(&repo, &[]),
                1,
                "3645-io/Not-Everything",
                &format!("origin {url}"),
            );
        }
        let repo = repo_with_origin("origin-none", None);
        assert_refused(
            &check_origin(&repo, &[]),
            1,
            "no origin remote",
            "no origin",
        );
        // NE_RELEASE_REPO names another repository, for testing a fork.
        let repo = repo_with_origin("origin-fork", Some("git@github.com:someone/fork.git"));
        let out = check_origin(&repo, &[("NE_RELEASE_REPO", "someone/fork")]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    }

    /// The script's `check_publish_tools` for `channel`, with the fake gh
    /// and origin the project's repository.
    fn check_publish_tools(channel: &str, env: &[(&str, &str)]) -> Output {
        let fakes = FakeTools::new("fake-gh", &[("gh", FAKE_GH)]);
        let repo = repo_with_origin(
            "publish-tools",
            Some("https://github.com/3645-io/Not-Everything.git"),
        );
        let body = format!(
            "ROOT='{}'\nCHANNEL={channel}\nRELEASE_REPO=${{NE_RELEASE_REPO:-3645-io/Not-Everything}}\n\
             check_publish_tools\necho tools ok",
            repo.dir.display()
        );
        let path = fakes.path();
        let mut all: Vec<(&str, &str)> = vec![("PATH", &path)];
        all.extend_from_slice(env);
        run_functions(
            &[
                "lower",
                "github_repo",
                "check_origin",
                "check_publish_tools",
            ],
            &body,
            &all,
        )
    }

    #[test]
    fn check_publish_tools_needs_gh_signed_in_and_pointing_at_the_project() {
        let project = ("FAKE_GH_REPO", "3645-io/Not-Everything");
        for channel in ["release", "rc"] {
            let out = check_publish_tools(channel, &[project]);
            assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
            assert_refused(
                &check_publish_tools(channel, &[project, ("FAKE_GH_AUTH", "out")]),
                1,
                "gh auth login",
                "gh signed out",
            );
            assert_refused(
                &check_publish_tools(channel, &[("FAKE_GH_REPO", "someone/fork")]),
                1,
                "someone/fork",
                "gh pointing at another repository",
            );
            assert_refused(
                &check_publish_tools(channel, &[]),
                1,
                "known GitHub host",
                "gh without a repository",
            );
        }
        // A beta's --publish only pushes the tag, so it needs no gh.
        let out = check_publish_tools("beta", &[("FAKE_GH_AUTH", "out")]);
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
    }

    #[test]
    fn check_no_release_stops_on_any_gh_error_but_release_not_found() {
        let fakes = FakeTools::new("fake-gh-release", &[("gh", FAKE_GH)]);
        let path = fakes.path();
        let run = |state: &str| {
            run_functions(
                &["check_no_release"],
                "check_no_release release/v0.3.0\necho no release",
                &[("PATH", &path), ("FAKE_GH_RELEASE", state)],
            )
        };
        let out = run("missing");
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert_refused(&run("exists"), 1, "already has a release", "a release");
        let out = run("error");
        assert_refused(&out, 1, "cannot tell", "a gh error");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("Bad credentials"), "{}", describe(&out));
    }

    #[test]
    fn check_notary_profile_stops_without_a_working_profile() {
        for (status, works) in [("0", true), ("69", false)] {
            let xcrun = format!(
                "[ \"$1 $2 $3 $4\" = 'notarytool history --keychain-profile not-everything-notary' ] || exit 3\nexit {status}"
            );
            let fakes = FakeTools::new("fake-xcrun", &[("xcrun", &xcrun)]);
            let path = fakes.path();
            let out = run_functions(
                &["check_notary_profile"],
                "PROFILE=not-everything-notary\ncheck_notary_profile\necho profile ok",
                &[("PATH", &path)],
            );
            if works {
                assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
            } else {
                assert_refused(&out, 1, "not-everything-notary", "a broken profile");
            }
        }
    }

    /// What `--resume` says when the signed app or command is missing.
    const MISSING: &str = "signed app or command is missing";
    /// What `--resume` says when the disk image Apple got is missing.
    const MISSING_DMG: &str = "disk image uploaded to Apple is missing";
    /// Where the disk image of [`dmg_uploaded_folder`] lives, under `work/`.
    const UPLOADED_DMG: &str = "dmg.Ab12Cd/Not-Everything-0.2.0-macos-arm64.dmg";

    /// Runs this checkout's script with `args` from the repository root.
    fn run_script(args: &[&str]) -> Output {
        run_with_timeout(
            Command::new("sh")
                .arg(script_path())
                .args(args)
                .current_dir(repo_root()),
        )
    }

    /// The script's `mount_point` function run on `output` (what `hdiutil
    /// attach` prints, warnings included).
    fn mount_point(output: &str) -> String {
        let function = shell_function(&script_text(), "mount_point")
            .unwrap_or_else(|| panic!("{SCRIPT} must define mount_point"));
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("{function}\nmount_point"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run sh");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(output.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", describe(&out));
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn mount_point_reads_hdiutil_attach_s_output() {
        // An APFS image, as hdiutil printed it on macOS 27 (with 2>&1).
        let apfs = "expected   CRC32 $C5801FD9\n\
            hdiutil: WARNING: 'hdiutil attach ...' is deprecated.\n\
            /dev/disk12         \tGUID_partition_scheme          \t\n\
            /dev/disk12s1       \tApple_APFS                     \t\n\
            /dev/disk13         \tEF57347C-0000-11AA-AA11-0030654\t\n\
            /dev/disk13s1       \t41504653-0000-11AA-AA11-0030654\t\
            /Volumes/Not-Everything Preview 0.2.0-RC-1\n";
        assert_eq!(
            mount_point(apfs),
            "/Volumes/Not-Everything Preview 0.2.0-RC-1\n"
        );
        let hfs = "/dev/disk4\tGUID_partition_scheme\t\n\
                   /dev/disk4s1\tApple_HFS\t/tmp/out/check/mount/Not-Everything 0.2.0 1\n";
        assert_eq!(
            mount_point(hfs),
            "/tmp/out/check/mount/Not-Everything 0.2.0 1\n"
        );
        assert_eq!(mount_point("/dev/disk4\tGUID_partition_scheme\t\n"), "\n");
    }

    /// A folder laid out as a first run leaves it while Apple processes the
    /// first upload: the signed app and command, the build's channel record
    /// and the saved submission. No disk image yet: it is built from the
    /// stapled app.
    fn uploaded_folder(tag: &str) -> PathBuf {
        let dir = temp_path(tag);
        let work = dir.join("work");
        fs::create_dir_all(work.join("Not-Everything.app/Contents")).unwrap();
        fs::create_dir_all(work.join("cli")).unwrap();
        fs::create_dir_all(dir.join("check")).unwrap();
        fs::write(work.join("cli/not-everything"), b"signed command").unwrap();
        fs::write(
            work.join("release-info"),
            "channel=release\nnumber=0\nversion=0.2.0\nlabel=0.2.0\n\
             tag=release/v0.2.0\ncommit=0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        fs::write(
            work.join("for-notary.json"),
            br#"{"id":"00000000-0000-0000-0000-000000000000","message":"Successfully uploaded file"}"#,
        )
        .unwrap();
        dir
    }

    /// A folder a run left while Apple processes the second upload: as
    /// [`uploaded_folder`], plus the disk image built from the stapled app,
    /// the record of its path and its saved submission.
    fn dmg_uploaded_folder(tag: &str) -> PathBuf {
        let dir = uploaded_folder(tag);
        let work = dir.join("work");
        let dmg = work.join(UPLOADED_DMG);
        fs::create_dir_all(dmg.parent().unwrap()).unwrap();
        fs::write(&dmg, b"signed disk image").unwrap();
        fs::write(
            work.join("for-notary-dmg.path"),
            format!("{}\n", dmg.display()),
        )
        .unwrap();
        fs::write(
            work.join("for-notary-dmg.json"),
            br#"{"id":"11111111-1111-1111-1111-111111111111","message":"Successfully uploaded file"}"#,
        )
        .unwrap();
        dir
    }

    /// Runs `--resume` on `dir`, removes it, and returns the output with
    /// whether the folder was left as it was.
    fn resume_and_compare(dir: &Path) -> (Output, bool) {
        let before = tree(dir);
        let out = run_script(&["--resume", dir.to_str().unwrap()]);
        let after = tree(dir);
        let _ = fs::remove_dir_all(dir);
        (out, before == after)
    }

    #[test]
    fn resume_needs_a_folder() {
        let out = run_script(&["--resume"]);
        assert_refused(
            &out,
            2,
            "usage:",
            "--resume without a folder is a usage error",
        );
    }

    #[test]
    fn rejects_an_unknown_option() {
        let out = run_script(&["--sign", "x"]);
        assert_refused(&out, 2, "usage:", "an unknown option is a usage error");
    }

    #[test]
    fn rejects_two_modes_or_a_mode_with_resume() {
        for args in [
            &["--rc", "--beta", "x"][..],
            &["--beta", "--rc", "x"],
            &["--resume", "x", "--rc"],
            &["--beta", "--resume", "x"],
            &["--rc", "x", "y"],
            &["--publish"],
        ] {
            let out = run_script(args);
            assert_refused(&out, 2, "usage:", &format!("{args:?} is a usage error"));
        }
    }

    #[test]
    fn resume_refuses_a_folder_that_does_not_exist() {
        let dir = temp_path("missing");
        let out = run_script(&["--resume", dir.to_str().unwrap()]);
        let created = dir.exists();
        let _ = fs::remove_dir_all(&dir);
        assert_refused(
            &out,
            1,
            "not a release folder",
            "--resume of a missing folder",
        );
        assert!(!created, "{SCRIPT} --resume must not create the folder");
    }

    #[test]
    fn resume_refuses_a_folder_with_no_saved_upload() {
        let dir = temp_path("noupload");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep me").unwrap();
        let before = tree(&dir);

        let out = run_script(&["--resume", dir.to_str().unwrap()]);

        let after = tree(&dir);
        let kept = fs::read_to_string(dir.join("keep.txt")).ok();
        let _ = fs::remove_dir_all(&dir);
        assert_refused(
            &out,
            1,
            "no upload to resume",
            "--resume with nothing uploaded",
        );
        assert_eq!(
            before, after,
            "{SCRIPT} --resume must leave the folder untouched"
        );
        assert_eq!(kept.as_deref(), Some("keep me"));
    }

    #[test]
    fn resume_refuses_a_folder_without_its_channel_record() {
        let dir = uploaded_folder("noinfo");
        fs::remove_file(dir.join("work/release-info")).unwrap();
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(
            &out,
            1,
            "no record of the build's channel",
            "--resume without work/release-info",
        );
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    #[test]
    fn resume_refuses_a_beta() {
        let dir = uploaded_folder("beta");
        fs::write(
            dir.join("work/release-info"),
            "channel=beta\nnumber=1\nversion=0.2.0\nlabel=0.2.0-beta-1\n\
             tag=beta/v0.2.0-beta-1\ncommit=0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(
            &out,
            1,
            "betas are not notarized",
            "--resume of a beta folder",
        );
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    #[test]
    fn resume_refuses_a_folder_that_already_holds_release_files() {
        for file in [
            "Not-Everything-0.2.0-macos-arm64.zip",
            "Not-Everything-Preview-0.2.0-RC-1-macos-arm64.zip",
            "Not-Everything-0.2.0-macos-arm64.dmg",
            "Not-Everything-Preview-0.2.0-RC-1-macos-arm64.dmg",
            "not-everything-preview-cli-0.2.0-RC-1-macos-arm64.tar.gz",
            "SHA256SUMS.txt",
        ] {
            let dir = uploaded_folder("done");
            fs::write(dir.join(file), b"done").unwrap();
            let (out, untouched) = resume_and_compare(&dir);
            assert_refused(
                &out,
                1,
                "already holds release files",
                &format!("--resume after {file} was written"),
            );
            assert!(
                untouched,
                "{SCRIPT} --resume must leave the folder untouched"
            );
        }
    }

    #[test]
    fn resume_refuses_a_folder_missing_the_signed_command() {
        let dir = uploaded_folder("nocli");
        fs::remove_file(dir.join("work/cli/not-everything")).unwrap();
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(&out, 1, MISSING, "--resume without the signed items");
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    /// Stopped while Apple held the first upload: the disk image is built
    /// later, from the stapled app, so a folder without one passes the item
    /// checks (and, here, stops at the fake app's missing Info.plist).
    #[test]
    fn resume_of_the_first_upload_needs_no_disk_image() {
        let dir = uploaded_folder("stage1");
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(&out, 1, "no version in", "--resume of the first upload");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("disk image"), "{}", describe(&out));
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    /// Stopped while Apple held the second upload: the recorded disk image
    /// is the one Apple got, so it is found again, never rebuilt.
    #[test]
    fn resume_of_the_second_upload_finds_the_recorded_disk_image() {
        let dir = dmg_uploaded_folder("stage2");
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(&out, 1, "no version in", "--resume of the second upload");
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    /// `--resume` never rebuilds an uploaded disk image, so the one recorded
    /// for the second upload must still be there.
    #[test]
    fn resume_of_the_second_upload_refuses_a_missing_disk_image() {
        for missing in [
            format!("work/{UPLOADED_DMG}"),
            "work/for-notary-dmg.path".into(),
        ] {
            let dir = dmg_uploaded_folder("nodmg");
            fs::remove_file(dir.join(&missing)).unwrap();
            let (out, untouched) = resume_and_compare(&dir);
            assert_refused(
                &out,
                1,
                MISSING_DMG,
                &format!("--resume of the second upload without {missing}"),
            );
            assert!(
                untouched,
                "{SCRIPT} --resume must leave the folder untouched"
            );
        }
    }

    /// A Preview folder holds the Preview app and command, so a folder with
    /// the release app's names is not one.
    #[test]
    fn resume_looks_for_the_preview_app_in_a_preview_folder() {
        let dir = uploaded_folder("preview");
        fs::write(
            dir.join("work/release-info"),
            "channel=rc\nnumber=1\nversion=0.2.0\nlabel=0.2.0-RC-1\n\
             tag=release/v0.2.0-RC-1\ncommit=0000000000000000000000000000000000000000\n",
        )
        .unwrap();
        let (out, untouched) = resume_and_compare(&dir);
        assert_refused(
            &out,
            1,
            MISSING,
            "--resume of an RC folder holding the release app's names",
        );
        assert!(
            untouched,
            "{SCRIPT} --resume must leave the folder untouched"
        );
    }

    // -- The release flow's git checks, in a throwaway repository. ---------

    /// A repository at `version` with the files the script reads, committed
    /// on `main`, and the script copied in.
    fn release_repo(tag: &str, version: &str) -> Sandbox {
        let repo = Sandbox::new(tag);
        repo.copy_from_repo(SCRIPT);
        repo.write(
            "Cargo.toml",
            &format!("[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"{version}\"\nrust-version = \"1.85\"\n"),
        );
        repo.write(
            "CHANGELOG.md",
            &format!("# Changelog\n\n## v{version}: Test release (2026-11-07)\n\nWhat changed.\n\n## v0.1.3: Older (2026-11-04)\n\nOld.\n"),
        );
        repo.write("LICENSE", "MIT\n");
        repo.write("docs/cli.md", "# cli\n");
        repo.commit_all("first");
        repo
    }

    /// Runs the sandbox's script with `args`, giving it a fresh output
    /// folder path as the last argument.
    fn plan(repo: &Sandbox, args: &[&str]) -> Output {
        let out_dir = repo.dir.join("target").join("out");
        let mut all: Vec<&str> = args.to_vec();
        let out_dir = out_dir.to_str().unwrap().to_owned();
        all.push(&out_dir);
        let out = repo.run(SCRIPT, &all);
        assert!(
            !repo.dir.join("target/out").exists(),
            "a refusal or a stop before building creates nothing\n{}",
            describe(&out)
        );
        out
    }

    /// The run passed every check and stopped before building, having
    /// printed the tag it would make.
    fn assert_planned(out: &Output, tag: &str, label: &str) {
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(tag),
            "plan must name {tag}\n{}",
            describe(out)
        );
        assert!(
            stdout.contains(label),
            "plan must name {label}\n{}",
            describe(out)
        );
        assert_refused(
            out,
            1,
            "cargo is not on PATH",
            "a planned run stops before building",
        );
    }

    fn ignore_target(repo: &Sandbox) {
        repo.write(".gitignore", "/target/\n");
        repo.commit_all("ignore target");
    }

    /// `repo` with a local bare copy of itself as origin, so fetching the
    /// tags works offline.
    fn add_local_origin(repo: &Sandbox) -> PathBuf {
        let bare = temp_path("origin");
        repo.git(&[
            "clone",
            "--quiet",
            "--bare",
            repo.dir.to_str().unwrap(),
            bare.to_str().unwrap(),
        ]);
        repo.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        bare
    }

    #[test]
    fn the_plan_prints_origin() {
        let repo = release_repo("plan-origin", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "dev"]);
        let bare = add_local_origin(&repo);
        let out = plan(&repo, &["--beta"]);
        let _ = fs::remove_dir_all(&bare);
        assert_planned(&out, "beta/v0.2.0-beta-1", "0.2.0-beta-1");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = format!("Origin:  {}", bare.display());
        assert!(stdout.contains(&line), "{}", describe(&out));
    }

    /// Publishing pushes the tag to origin, so an origin that is not the
    /// project's repository is refused before anything is fetched or built.
    #[test]
    fn publish_refuses_an_origin_that_is_not_the_project_s() {
        let repo = release_repo("publish-origin", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "dev"]);
        let bare = add_local_origin(&repo);
        let out = plan(&repo, &["--beta", "--publish"]);
        let _ = fs::remove_dir_all(&bare);
        assert_refused(
            &out,
            1,
            "3645-io/Not-Everything",
            "--publish with a local origin",
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(!stdout.contains("== Plan"), "{}", describe(&out));
    }

    #[test]
    fn rc_refuses_any_branch_but_the_version_s_rc_branch() {
        let repo = release_repo("rc-branch", "0.2.0");
        ignore_target(&repo);
        for branch in ["main", "rc/0.1.9", "rc/0.2.0-x", "dev"] {
            if branch != "main" {
                repo.git(&["checkout", "--quiet", "-B", branch]);
            }
            let out = plan(&repo, &["--rc"]);
            assert_refused(&out, 1, "rc/0.2.0", &format!("--rc on {branch}"));
        }
    }

    #[test]
    fn every_mode_refuses_uncommitted_changes() {
        let repo = release_repo("dirty", "0.2.0");
        ignore_target(&repo);
        for (branch, mode) in [("rc/0.2.0", "--rc"), ("dev", "--beta"), ("main", "")] {
            repo.git(&["checkout", "--quiet", "-B", branch]);
            repo.write("notes.txt", "not committed\n");
            let args: Vec<&str> = [mode].into_iter().filter(|m| !m.is_empty()).collect();
            let out = plan(&repo, &args);
            assert_refused(
                &out,
                1,
                "uncommitted",
                &format!("{mode:?} with a dirty tree"),
            );
            fs::remove_file(repo.dir.join("notes.txt")).unwrap();
        }
    }

    #[test]
    fn rc_starts_at_one_and_counts_on_from_the_highest_rc() {
        let repo = release_repo("rc-number", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        let out = plan(&repo, &["--rc"]);
        assert_planned(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1");

        for n in ["1", "2", "10"] {
            repo.write("src.txt", n);
            repo.commit_all(&format!("change {n}"));
            repo.git(&["tag", "-a", &format!("release/v0.2.0-RC-{n}"), "-m", n]);
        }
        // Tags of other versions and kinds do not count.
        repo.git(&["tag", "release/v0.1.9-RC-40", "HEAD~1"]);
        repo.git(&["tag", "beta/v0.2.0-beta-30", "HEAD~1"]);
        repo.write("src.txt", "new");
        repo.commit_all("after RC 10");
        let out = plan(&repo, &["--rc"]);
        assert_planned(&out, "release/v0.2.0-RC-11", "0.2.0-RC-11");
    }

    #[test]
    fn rc_refuses_a_commit_that_already_has_an_rc() {
        let repo = release_repo("rc-twice", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        let out = plan(&repo, &["--rc"]);
        assert_refused(
            &out,
            1,
            "already release candidate",
            "--rc of a tagged commit",
        );
    }

    #[test]
    fn rc_refuses_a_version_already_released() {
        let repo = release_repo("rc-released", "0.2.0");
        ignore_target(&repo);
        repo.git(&["tag", "-a", "release/v0.2.0", "-m", "released"]);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.commit_all("later");
        let out = plan(&repo, &["--rc"]);
        assert_refused(&out, 1, "already released", "--rc after the release");
    }

    #[test]
    fn beta_refuses_any_branch_but_dev() {
        let repo = release_repo("beta-branch", "0.2.0");
        ignore_target(&repo);
        for branch in ["main", "rc/0.2.0", "feature/x"] {
            if branch != "main" {
                repo.git(&["checkout", "--quiet", "-B", branch]);
            }
            let out = plan(&repo, &["--beta"]);
            assert_refused(&out, 1, "dev", &format!("--beta on {branch}"));
        }
    }

    #[test]
    fn beta_counts_on_from_the_highest_beta() {
        let repo = release_repo("beta-number", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "dev"]);
        let out = plan(&repo, &["--beta"]);
        assert_planned(&out, "beta/v0.2.0-beta-1", "0.2.0-beta-1");
        repo.git(&["tag", "-a", "beta/v0.2.0-beta-1", "-m", "b1"]);
        repo.git(&["tag", "-a", "beta/v0.2.0-beta-4", "-m", "b4"]);
        repo.git(&["tag", "-a", "release/v0.2.0-RC-9", "-m", "rc"]);
        let out = plan(&repo, &["--beta"]);
        assert_planned(&out, "beta/v0.2.0-beta-5", "0.2.0-beta-5");
    }

    /// `main` after the release PR: the latest RC merged with a merge
    /// commit, so its tree is `main`'s.
    fn released_main(tag: &str) -> Sandbox {
        let repo = release_repo(tag, "0.1.3");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n",
        );
        repo.write(
            "CHANGELOG.md",
            "# Changelog\n\n## v0.2.0: Test release (2026-11-07)\n\nWhat changed.\n",
        );
        repo.commit_all("0.2.0");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        repo.write("fix.txt", "fix\n");
        repo.commit_all("fix");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-2", "-m", "rc 2"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.git(&[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Release 0.2.0",
            "rc/0.2.0",
        ]);
        repo
    }

    #[test]
    fn release_builds_main_from_the_latest_rc() {
        let repo = released_main("release-ok");
        let out = plan(&repo, &[]);
        assert_planned(&out, "release/v0.2.0", "0.2.0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("release/v0.2.0-RC-2"), "{}", describe(&out));
    }

    #[test]
    fn release_refuses_any_branch_but_main() {
        let repo = released_main("release-branch");
        for branch in ["rc/0.2.0", "dev"] {
            repo.git(&["checkout", "--quiet", "-B", branch]);
            let out = plan(&repo, &[]);
            assert_refused(&out, 1, "main", &format!("a release from {branch}"));
        }
    }

    #[test]
    fn release_refuses_without_a_release_candidate() {
        let repo = release_repo("release-norc", "0.2.0");
        ignore_target(&repo);
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "no release candidate", "a release with no RC");
    }

    #[test]
    fn release_refuses_main_that_differs_from_the_latest_rc() {
        let repo = released_main("release-differs");
        repo.write("late.txt", "after the RC\n");
        repo.commit_all("late change");
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "release/v0.2.0-RC-2", "main changed after the RC");

        // An older RC does not count once a newer one exists.
        let repo = released_main("release-older");
        repo.git(&[
            "tag",
            "-a",
            "release/v0.2.0-RC-3",
            "-m",
            "rc 3",
            "rc/0.2.0~1",
        ]);
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "release/v0.2.0-RC-3", "a newer RC than main's");
    }

    #[test]
    fn release_refuses_an_rc_that_main_does_not_contain() {
        let repo = release_repo("release-ancestor", "0.1.3");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n",
        );
        repo.write(
            "CHANGELOG.md",
            "# Changelog\n\n## v0.2.0: Test release (2026-11-07)\n\nWhat changed.\n",
        );
        repo.commit_all("0.2.0");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-1", "-m", "rc 1"]);
        // The same tree committed on main without merging the RC.
        repo.git(&["checkout", "--quiet", "main"]);
        repo.git(&["checkout", "rc/0.2.0", "--", "."]);
        repo.commit_all("copy, not merge");
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "not merged", "an RC main does not contain");
    }

    #[test]
    fn release_refuses_a_version_already_released() {
        let repo = released_main("release-twice");
        repo.git(&["tag", "-a", "release/v0.2.0", "-m", "released"]);
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "already released", "a second release of 0.2.0");
    }

    #[test]
    fn release_needs_the_version_s_changelog_entry() {
        let repo = released_main("release-changelog");
        repo.git(&["checkout", "--quiet", "rc/0.2.0"]);
        repo.write("CHANGELOG.md", "# Changelog\n\n## v0.1.3: Older\n\nOld.\n");
        repo.commit_all("no entry");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-3", "-m", "rc 3"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.git(&[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Release 0.2.0",
            "rc/0.2.0",
        ]);
        let out = plan(&repo, &[]);
        assert_refused(&out, 1, "CHANGELOG.md", "a release without its notes");
    }

    #[test]
    fn every_mode_refuses_a_version_that_is_not_x_y_z() {
        for version in ["0.2", "0.2.0-alpha", "0.100.0", "0.2.00", "v0.2.0"] {
            let repo = release_repo("bad-version", version);
            ignore_target(&repo);
            repo.git(&["checkout", "--quiet", "-b", "dev"]);
            let out = plan(&repo, &["--beta"]);
            assert_refused(&out, 1, "X.Y.Z", &format!("version {version:?}"));
        }
    }

    #[test]
    fn a_new_release_refuses_an_existing_folder_and_leaves_it_untouched() {
        let repo = release_repo("exists", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "dev"]);
        let dir = temp_path("exists");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep me").unwrap();
        let before = tree(&dir);

        let out = repo.run(SCRIPT, &["--beta", dir.to_str().unwrap()]);

        let after = tree(&dir);
        let _ = fs::remove_dir_all(&dir);
        assert_refused(&out, 1, "already exists", "an existing output folder");
        assert_eq!(before, after, "{SCRIPT} must not touch an existing folder");
    }

    // -- The performance table (--perf). -----------------------------------

    /// What the script says when `--publish` of an RC or a release has no
    /// `--perf`.
    const NEEDS_PERF: &str = "needs --perf PERF_DIR";
    /// What it says when the run in PERF_DIR has a failing or unmeasured
    /// metric.
    const DID_NOT_PASS: &str = "did not pass every metric";
    /// The last line of a passing run's `summary.md`.
    const PASSING: &str =
        "perf-compare: 30 pass, 0 fail, 0 not measured, 1 no baseline, 2 tracked, 0 skipped";

    /// Writes a finished `scripts/perf-check.sh --tier rc` run into `dir`:
    /// `metrics.txt`, `summary.md` ending with `last`, and `run.txt` whose
    /// `git:` line is `git`. Returns `dir` as a string.
    fn write_perf_run(dir: &Path, git: &str, last: &str) -> String {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("metrics.txt"), "startup_ms=210\nstartup_ms=215\n").unwrap();
        fs::write(
            dir.join("summary.md"),
            format!(
                "# Performance gate: rc tier\n\n| Metric | Verdict |\n| --- | --- |\n\n{last}\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("run.txt"),
            format!("tier: rc\nsteps: startup dmg\nmachine: test\ncache: warm\ngit: {git}\nrustc: test\n"),
        )
        .unwrap();
        dir.display().to_string()
    }

    /// A passing run of `git` named `name` in `repo`'s ignored `target/`.
    fn perf_run(repo: &Sandbox, name: &str, git: &str) -> String {
        write_perf_run(&repo.dir.join("target").join(name), git, PASSING)
    }

    /// An `rc/0.2.0` branch ready for RC 1 whose previous release, 0.1.3,
    /// has its measured values committed, and its HEAD.
    fn rc_with_previous_release(tag: &str) -> (Sandbox, String) {
        let repo = release_repo(tag, "0.2.0");
        ignore_target(&repo);
        repo.git(&["tag", "-a", "release/v0.1.3", "-m", "0.1.3", "HEAD~1"]);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        repo.write(
            "docs/gates/releases/0.1.3.txt",
            "# 0.1.3, the release's values\nstartup_ms=230\n",
        );
        let head = repo.commit_all("0.1.3's values");
        (repo, head)
    }

    /// The run passed every check, `--perf` included, and stopped before
    /// building; its Plan names the run and the previous release.
    fn assert_planned_with_perf(out: &Output, tag: &str, label: &str, previous: &str) {
        assert_planned(out, tag, label);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Performance: ")
                && stdout.contains(&format!("compared with {previous}")),
            "the Plan must name the run and {previous}\n{}",
            describe(out)
        );
    }

    #[test]
    fn perf_takes_one_folder_and_no_beta() {
        let repo = release_repo("perf-usage", "0.2.0");
        for args in [
            &["--rc", "x", "--perf"][..],
            &["--rc", "--perf", "--publish", "x"],
            &["--rc", "--perf", "", "x"],
            &["--rc", "--perf", "a", "--perf", "a", "x"],
            &["--perf", "a", "--rc", "x", "--perf", "b"],
            &["--beta", "--perf", "a", "x"],
            &["--perf", "a", "--beta", "x"],
            &["--resume", "x", "--perf"],
            &["--resume", "x", "--perf", "a", "--perf", "a"],
        ] {
            let out = repo.run(SCRIPT, args);
            assert_refused(&out, 2, "usage:", &format!("{args:?} is a usage error"));
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("[--perf PERF_DIR]"), "{}", describe(&out));
        }
    }

    /// A release candidate's and a release's GitHub page carries the table,
    /// so `--publish` without `--perf` is refused before anything else.
    #[test]
    fn publishing_a_release_candidate_or_a_release_needs_perf() {
        let repo = release_repo("perf-needed-rc", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        let release = released_main("perf-needed-release");
        for (repo, args) in [
            (&repo, &["--rc", "--publish"][..]),
            (&repo, &["--publish", "--rc"][..]),
            (&release, &["--publish"][..]),
        ] {
            let out = plan(repo, args);
            assert_refused(&out, 1, NEEDS_PERF, &format!("{args:?} without --perf"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(!stdout.contains("== Plan"), "{}", describe(&out));
        }
    }

    /// A beta has no GitHub page: it never needs `--perf`, published or not.
    #[test]
    fn a_beta_never_needs_perf() {
        let repo = release_repo("perf-beta", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "dev"]);
        let out = plan(&repo, &["--beta"]);
        assert_planned(&out, "beta/v0.2.0-beta-1", "0.2.0-beta-1");
        let bare = add_local_origin(&repo);
        let out = plan(&repo, &["--beta", "--publish"]);
        let _ = fs::remove_dir_all(&bare);
        // Refused for its local origin, not for --perf.
        assert_refused(
            &out,
            1,
            "3645-io/Not-Everything",
            "--beta --publish with a local origin",
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("--perf"), "{}", describe(&out));
    }

    /// Without `--publish`, `--perf` is optional and checked the same way.
    /// A run is refused unless it is a finished rc-tier run, from a clean
    /// tree, of the commit being built or one with the same tree, whose last
    /// `perf-compare:` line has no failing metric. Unmeasured metrics pass
    /// here: the DMG size is measured only after the build, when
    /// `perf_section` judges the whole tier.
    #[test]
    fn perf_refuses_a_run_that_is_not_a_passing_run_of_the_commit() {
        let (repo, head) = rc_with_previous_release("perf-run");
        let head = head.as_str();
        let other = "1".repeat(40);
        let other = other.as_str();
        let dirty = format!("{head} (uncommitted changes)");
        let dirty = dirty.as_str();
        let line = |fail: u32, not_measured: u32| {
            format!(
                "perf-compare: 30 pass, {fail} fail, {not_measured} not measured, 0 no baseline, 2 tracked, 0 skipped"
            )
        };
        // (what, run.txt's git: line, summary.md's last line, a file to
        // remove, the refusal).
        let cases = [
            (
                "no metrics.txt",
                head,
                PASSING.to_owned(),
                Some("metrics.txt"),
                "has no metrics.txt",
            ),
            (
                "no summary.md",
                head,
                PASSING.to_owned(),
                Some("summary.md"),
                "has no summary.md",
            ),
            (
                "no run.txt",
                head,
                PASSING.to_owned(),
                Some("run.txt"),
                "has no run.txt",
            ),
            (
                "another commit",
                other,
                PASSING.to_owned(),
                None,
                "the commit being built",
            ),
            (
                "uncommitted changes",
                dirty,
                PASSING.to_owned(),
                None,
                "uncommitted changes",
            ),
            ("a failing metric", head, line(1, 0), None, DID_NOT_PASS),
            ("ten failing metrics", head, line(10, 0), None, DID_NOT_PASS),
            (
                "no perf-compare line",
                head,
                "No verdicts.".to_owned(),
                None,
                DID_NOT_PASS,
            ),
        ];
        for (i, (what, git, last, remove, phrase)) in cases.into_iter().enumerate() {
            let dir = repo.dir.join("target").join(format!("perf-{i}"));
            let perf = write_perf_run(&dir, git, &last);
            if let Some(file) = remove {
                fs::remove_file(dir.join(file)).unwrap();
            }
            let out = plan(&repo, &["--rc", "--perf", &perf]);
            assert_refused(&out, 1, phrase, &format!("a run with {what}"));
        }

        // Unmeasured metrics (the DMG size, before the build) pass here.
        for not_measured in [1, 10] {
            let dir = repo
                .dir
                .join(format!("target/perf-unmeasured-{not_measured}"));
            let perf = write_perf_run(&dir, head, &line(0, not_measured));
            let out = plan(&repo, &["--rc", "--perf", &perf]);
            assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        }

        // Another tier.
        let perf = perf_run(&repo, "perf-pr", head);
        let run = Path::new(&perf).join("run.txt");
        let text = fs::read_to_string(&run)
            .unwrap()
            .replace("tier: rc", "tier: pr");
        fs::write(&run, text).unwrap();
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(&out, 1, "--tier rc", "a pr-tier run");

        // Not a folder.
        let missing = repo.dir.join("target/no-such-run").display().to_string();
        let out = plan(&repo, &["--rc", "--perf", &missing]);
        assert_refused(&out, 1, "is not a folder", "a missing run");

        // Only the last perf-compare line counts.
        let dir = repo.dir.join("target/perf-rerun");
        let perf = write_perf_run(&dir, head, PASSING);
        fs::write(
            dir.join("summary.md"),
            format!("{}\n\n{PASSING}\n", line(1, 1)),
        )
        .unwrap();
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        fs::write(
            dir.join("summary.md"),
            format!("{PASSING}\n\n{}\n", line(1, 0)),
        )
        .unwrap();
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(&out, 1, DID_NOT_PASS, "a failing last line");

        // A passing run of HEAD, also given by a relative path.
        let perf = perf_run(&repo, "perf-ok", head);
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        let out_dir = repo.dir.join("target/out").display().to_string();
        let out = repo.run(SCRIPT, &["--rc", "--perf", "target/perf-ok", &out_dir]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        // The temporary folder may sit behind a symbolic link (/var).
        let full = fs::canonicalize(&perf).unwrap().display().to_string();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!("Performance: {full},")),
            "the Plan names the run's full path\n{}",
            describe(&out)
        );
    }

    /// A run of another commit with the same code (its tree) passes, so a
    /// release built from the merged release PR can use its candidate's
    /// run; a run of other code is refused.
    #[test]
    fn perf_accepts_a_run_of_a_commit_with_the_same_code() {
        let (repo, measured) = rc_with_previous_release("perf-same-tree");
        let perf = perf_run(&repo, "perf-same", &measured);
        repo.git(&["commit", "--quiet", "--allow-empty", "-m", "same code"]);
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        repo.write("src/changed.rs", "// changed\n");
        repo.commit_all("other code");
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(&out, 1, "its code differs", "a run of other code");
    }

    /// What the script says when the commit being built differs from the
    /// measured one only in files no build compiles.
    const SAME_CODE: &str = "differs from it only in documents, scripts and tests";

    /// A run of a commit that differs from the one being built only in
    /// documents (top-level `*.md` but THIRD-PARTY-NOTICES.md, which the app
    /// and the command embed; crates' READMEs; `docs/`), scripts, CI, agent
    /// rules and tests (`crates/*/tests/`) measured the same app, command and
    /// benchmarks, so it passes: a change to the release tooling or to the
    /// gate's baseline needs no second run. A change to anything else is
    /// refused.
    #[test]
    fn perf_accepts_a_run_whose_commit_differs_only_in_docs_scripts_and_tests() {
        let (repo, measured) = rc_with_previous_release("perf-docs-only");
        let perf = perf_run(&repo, "perf-docs", &measured);
        for (path, text) in [
            ("docs/gates/perf-baseline.toml", "# moved\n"),
            ("scripts/tool.sh", "#!/bin/sh\n"),
            (".github/workflows/tool.yml", "name: tool\n"),
            (".agents/rules/tool.md", "# Tool\n"),
            ("crates/fx-tool/tests/tool.rs", "// a test\n"),
            ("crates/fx-tool/README.md", "# fx-tool\n"),
            ("TOOL.md", "# Tool\n"),
        ] {
            repo.write(path, text);
        }
        repo.commit_all("documents, scripts and tests");
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(SAME_CODE),
            "the script must say why it takes the run\n{}",
            describe(&out)
        );

        for (path, what) in [
            ("crates/fx-tool/src/lib.rs", "a crate's code"),
            ("crates/fx-tool/examples/bench.rs", "a benchmark"),
            ("crates/fx-tool/Cargo.toml", "a crate's manifest"),
            ("crates/fx-tool/assets/icon.svg", "an asset"),
            ("crates/fx-tool/src/notes.md", "a document a crate embeds"),
            ("THIRD-PARTY-NOTICES.md", "the notices the app embeds"),
            ("Cargo.lock", "the lock file"),
        ] {
            repo.write(path, "changed\n");
            repo.commit_all(what);
            let out = plan(&repo, &["--rc", "--perf", &perf]);
            assert_refused(&out, 1, "its code differs", &format!("a change to {what}"));
            repo.git(&["reset", "--quiet", "--hard", "HEAD~1"]);
        }
    }

    // -- On targets only (--targets-only). ---------------------------------

    /// What the script says when, on targets only, a metric misses its
    /// target.
    const MISSES_TARGET: &str = "misses its target";
    /// What it says when the failing metrics can't be told from the report.
    const CANNOT_TELL: &str = "cannot tell which metrics failed";
    /// The table's header in perf-compare's report.
    const TABLE_HEAD: &str = "| ID | Metric | Baseline | Measured | Change | Target | Verdict |\n\
                              | --- | --- | --- | --- | --- | --- | --- |\n";

    /// A finished rc-tier run of `git` in `repo`'s `target/name` whose
    /// `summary.md` holds `rows` (perf-compare's table) and `last`.
    fn table_run(repo: &Sandbox, name: &str, git: &str, rows: &str, last: &str) -> String {
        let dir = repo.dir.join("target").join(name);
        let perf = write_perf_run(&dir, git, last);
        fs::write(
            dir.join("summary.md"),
            format!("# Performance gate: rc tier\n\n{TABLE_HEAD}{rows}\n{last}\n"),
        )
        .unwrap();
        perf
    }

    /// Rows of perf-compare's table: one passing metric, two that failed
    /// only their regression check, a tracked one that would fail both, and
    /// the DMG size, which is measured after the build.
    const REGRESSED_ROWS: &str = "\
| S1 | Search, 2M files (p95) | 7.136 ms | 7.279 ms | +2.0% | <= 16 ms | PASS |
| S7 | Scroll frame time (p95) | 1.12 ms | 2.78 ms | +148.2% | <= 8.3 ms | FAIL (regression) |
| F6 | Catch-up after a relaunch | 503.312 ms | 564.545 ms | +12.2% | <= 10000 ms | FAIL (regression) |
| F5 | First results during the first scan | 12514 ms | 12755 ms | +1.9% | <= 2000 ms | TRACKED (would be FAIL: target, regression) |
| O1 | DMG size | 4334877 bytes | - | - | - | NOT MEASURED (no dmg_bytes samples) |
";
    /// The count line under [`REGRESSED_ROWS`].
    const REGRESSED_LINE: &str =
        "perf-compare: 1 pass, 2 fail, 1 not measured, 0 no baseline, 1 tracked, 0 skipped";

    /// `--targets-only` judges `--perf`'s run, so it needs `--perf` (which a
    /// beta never takes), and comes once.
    #[test]
    fn targets_only_needs_perf_and_comes_once() {
        let repo = release_repo("targets-usage", "0.2.0");
        for args in [
            &["--rc", "--targets-only", "x"][..],
            &["--targets-only", "x"],
            &["--beta", "--targets-only", "x"],
            &[
                "--rc",
                "--perf",
                "a",
                "--targets-only",
                "--targets-only",
                "x",
            ],
            &["--resume", "x", "--targets-only"],
        ] {
            let out = repo.run(SCRIPT, args);
            assert_refused(&out, 2, "usage:", &format!("{args:?} is a usage error"));
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("[--targets-only]"), "{}", describe(&out));
        }
    }

    /// With `--targets-only`, a run whose failing metrics failed only their
    /// regression check (each still meets its target) passes, and the Plan
    /// names them; without it, the same run is refused. A tracked metric
    /// that would miss its target doesn't count, as without the option.
    #[test]
    fn targets_only_passes_regressions_that_meet_their_targets() {
        let (repo, head) = rc_with_previous_release("targets-pass");
        let perf = table_run(
            &repo,
            "perf-regressed",
            &head,
            REGRESSED_ROWS,
            REGRESSED_LINE,
        );
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(&out, 1, DID_NOT_PASS, "regressions without --targets-only");

        for args in [
            &["--rc", "--perf", &perf, "--targets-only"][..],
            &["--rc", "--targets-only", "--perf", &perf],
        ] {
            let out = plan(&repo, args);
            assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("judged on targets only")
                    && stdout.contains("beyond their allowance: S7 F6"),
                "the Plan must say the run is judged on targets and name S7 and F6\n{}",
                describe(&out)
            );
        }

        // A passing run says it is judged on targets, with nothing beyond.
        let perf = perf_run(&repo, "perf-passing", &head);
        let out = plan(&repo, &["--rc", "--perf", &perf, "--targets-only"]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.3");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("judged on targets only") && !stdout.contains("beyond"),
            "{}",
            describe(&out)
        );
    }

    /// With `--targets-only`, a metric that misses its target is refused,
    /// naming it, and so is a run whose failing metrics the report doesn't
    /// show (a count of failures with fewer FAIL rows).
    #[test]
    fn targets_only_refuses_a_missed_target_or_an_unclear_report() {
        let (repo, head) = rc_with_previous_release("targets-refuse");
        let missed = "\
| S3 | Regex search, 2M files (p95) | 53.756 ms | 101.2 ms | +88.3% | <= 100 ms | FAIL (target, regression) |
| S7 | Scroll frame time (p95) | 1.12 ms | 2.78 ms | +148.2% | <= 8.3 ms | FAIL (regression) |
";
        let target_only = "\
| M1 | Memory, window hidden | 210 MB | 210 MB | +0.0% | <= 200 MB | FAIL (target) |
";
        let line = |fail: u32| {
            format!(
                "perf-compare: 20 pass, {fail} fail, 1 not measured, 0 no baseline, 2 tracked, 0 skipped"
            )
        };
        for (i, (rows, last, phrase, id)) in [
            (missed, line(2), MISSES_TARGET, "S3"),
            (target_only, line(1), MISSES_TARGET, "M1"),
            ("", line(10), CANNOT_TELL, ""),
            (REGRESSED_ROWS, line(3), CANNOT_TELL, ""),
            ("", "No verdicts.".to_owned(), DID_NOT_PASS, ""),
        ]
        .into_iter()
        .enumerate()
        {
            let perf = table_run(&repo, &format!("perf-refused-{i}"), &head, rows, &last);
            let out = plan(&repo, &["--rc", "--perf", &perf, "--targets-only"]);
            assert_refused(&out, 1, phrase, &format!("case {i}"));
            assert!(
                String::from_utf8_lossy(&out.stderr).contains(id),
                "the refusal must name {id}\n{}",
                describe(&out)
            );
        }
    }

    /// `check_targets` reads perf-compare's report: it prints the IDs of the
    /// metrics that failed only their regression check, refuses a missed
    /// target, and, asked for `all`, an unmeasured gated metric (the DMG
    /// size is measured by then). Only the last section counts.
    #[test]
    fn check_targets_reads_the_last_section_of_the_report() {
        let dir = temp_path("check-targets");
        fs::create_dir_all(&dir).unwrap();
        let report = dir.join("report.md");
        let run = |text: &str, which: &str| {
            fs::write(&report, text).unwrap();
            let body = format!("check_targets '{}' {which}", report.display());
            run_functions(&["check_targets"], &body, &[])
        };
        let regressed = format!("{TABLE_HEAD}{REGRESSED_ROWS}\n{REGRESSED_LINE}\n");
        let out = run(&regressed, "before");
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "S7 F6");
        let out = run(&regressed, "all");
        assert_refused(&out, 1, "not measured", "an unmeasured metric with all");

        let measured = regressed
            .replace(
                "| - | - | - | NOT MEASURED (no dmg_bytes samples) |",
                "| 4400000 bytes | +1.5% | - | PASS |",
            )
            .replace("1 not measured", "0 not measured");
        let out = run(&measured, "all");
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "S7 F6");

        // An earlier section's failures don't count; the last one's do.
        let passing =
            "perf-compare: 30 pass, 0 fail, 0 not measured, 0 no baseline, 0 tracked, 0 skipped";
        let out = run(&format!("{regressed}\n{TABLE_HEAD}\n{passing}\n"), "all");
        assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");
        let out = run(&format!("{TABLE_HEAD}\n{passing}\n\n{regressed}"), "before");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "S7 F6");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The table compares with the previous release, the highest
    /// `release/vX.Y.Z` tag below the version (by version, not text; release
    /// candidates' tags don't count), whose measured values must be in
    /// `docs/gates/releases/<X.Y.Z>.txt`.
    #[test]
    fn perf_needs_the_previous_release_s_measured_values() {
        let repo = release_repo("perf-previous", "0.2.0");
        ignore_target(&repo);
        repo.git(&["checkout", "--quiet", "-b", "rc/0.2.0"]);
        let head = repo.git(&["rev-parse", "HEAD"]);
        let perf = perf_run(&repo, "perf-1", &head);
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(&out, 1, "no release before 0.2.0", "no earlier release");

        for tag in [
            "release/v0.1.9",
            "release/v0.1.10",
            "release/v0.1.11-RC-1",
            "release/v0.2.1",
            "release/v1.0.0",
        ] {
            repo.git(&["tag", "-a", tag, "-m", tag, "HEAD~1"]);
        }
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_refused(
            &out,
            1,
            "docs/gates/releases/0.1.10.txt",
            "0.1.10's values missing",
        );

        repo.write("docs/gates/releases/0.1.9.txt", "startup_ms=240\n");
        repo.write(
            "docs/gates/releases/0.1.10.txt",
            "# 0.1.10\nstartup_ms=230\n",
        );
        let head = repo.commit_all("the releases' values");
        let perf = perf_run(&repo, "perf-2", &head);
        let out = plan(&repo, &["--rc", "--perf", &perf]);
        assert_planned_with_perf(&out, "release/v0.2.0-RC-1", "0.2.0-RC-1", "0.1.10");
    }

    /// A release builds `main` after the release PR. Its release
    /// candidate's run passes while main's merge has the same code (tree),
    /// so the release needs no second 30-minute run; once the code differs,
    /// it needs a run of its own.
    #[test]
    fn a_release_can_use_its_candidate_s_run_for_the_same_code() {
        let repo = released_main("perf-release");
        repo.git(&["tag", "-a", "release/v0.1.3", "-m", "0.1.3", "main~1"]);
        repo.git(&["checkout", "--quiet", "rc/0.2.0"]);
        repo.write("docs/gates/releases/0.1.3.txt", "startup_ms=230\n");
        let rc = repo.commit_all("0.1.3's values");
        repo.git(&["tag", "-a", "release/v0.2.0-RC-3", "-m", "rc 3"]);
        repo.git(&["checkout", "--quiet", "main"]);
        repo.git(&[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Release 0.2.0",
            "rc/0.2.0",
        ]);
        let main = repo.git(&["rev-parse", "HEAD"]);
        assert_eq!(
            repo.git(&["rev-parse", "HEAD^{tree}"]),
            repo.git(&["rev-parse", &format!("{rc}^{{tree}}")]),
            "the release PR's merge has the candidate's code"
        );

        let rc_run = perf_run(&repo, "perf-rc", &rc);
        let out = plan(&repo, &["--perf", &rc_run]);
        assert_planned_with_perf(&out, "release/v0.2.0", "0.2.0", "0.1.3");
        let main_run = perf_run(&repo, "perf-main", &main);
        let out = plan(&repo, &["--perf", &main_run]);
        assert_planned_with_perf(&out, "release/v0.2.0", "0.2.0", "0.1.3");

        repo.write("src/late.rs", "// a change after the candidate\n");
        repo.commit_all("late change");
        let out = plan(&repo, &["--perf", &rc_run]);
        assert_refused(
            &out,
            1,
            "its code differs",
            "the candidate's run for different code",
        );
    }

    /// `--resume --publish` of a release or an RC needs `--perf` too, and
    /// its run must be of the build's recorded commit; the folder is left as
    /// it was.
    #[test]
    fn resume_publish_needs_perf_of_the_recorded_commit() {
        let dir = uploaded_folder("perf-resume");
        let runs = temp_path("perf-resume-runs");
        let zero = "0".repeat(40);
        let other = write_perf_run(&runs.join("other"), &"1".repeat(40), PASSING);
        let dirty = write_perf_run(
            &runs.join("dirty"),
            &format!("{zero} (uncommitted changes)"),
            PASSING,
        );
        let failing = write_perf_run(
            &runs.join("failing"),
            &zero,
            "perf-compare: 29 pass, 1 fail, 0 not measured, 0 no baseline, 0 tracked, 0 skipped",
        );
        let folder = dir.to_str().unwrap();
        let before = tree(&dir);
        for (args, phrase) in [
            (vec!["--resume", folder, "--publish"], NEEDS_PERF),
            (
                vec!["--resume", folder, "--perf", other.as_str(), "--publish"],
                "the commit being built",
            ),
            (
                vec!["--resume", folder, "--perf", dirty.as_str()],
                "uncommitted changes",
            ),
            (
                vec!["--resume", folder, "--perf", failing.as_str()],
                DID_NOT_PASS,
            ),
        ] {
            let out = run_script(&args);
            assert_refused(&out, 1, phrase, &format!("{args:?}"));
            assert_eq!(
                before,
                tree(&dir),
                "{SCRIPT} --resume must leave the folder untouched"
            );
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&runs);
    }

    /// `release_notes`: a release candidate's note, the CHANGELOG.md entry
    /// and, with `--perf`, one blank line and the table; without `--perf`
    /// the notes are the entry as before.
    #[test]
    fn release_notes_put_the_table_after_the_changelog_entry() {
        let dir = temp_path("notes");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("CHANGELOG.md"),
            "# Changelog\n\n## v0.2.0: Test release (2026-11-07)\n\nWhat changed.\n\n- One.\n  \n- Two.\n\n\n## v0.1.3: Older\n\nOld.\n",
        )
        .unwrap();
        fs::write(
            dir.join("performance.md"),
            "## Performance\n\n| Metric | 0.2.0 |\n",
        )
        .unwrap();
        let notes = |channel: &str, perf: &str| {
            let body = format!(
                "ROOT='{d}'\nOUT='{d}'\nVERSION=0.2.0\nNUMBER=2\nCHANNEL={channel}\nPERF_DIR={perf}\nrelease_notes",
                d = dir.display()
            );
            let out = run_functions(&["changelog_entry", "release_notes"], &body, &[]);
            assert_eq!(out.status.code(), Some(0), "{}", describe(&out));
            String::from_utf8(out.stdout).unwrap()
        };
        let entry = "\nWhat changed.\n\n- One.\n  \n- Two.\n";
        let table = "## Performance\n\n| Metric | 0.2.0 |\n";
        assert_eq!(
            notes("release", ""),
            format!("{entry}\n\n"),
            "no --perf: the entry as before"
        );
        assert_eq!(notes("release", "/perf"), format!("{entry}\n{table}"));
        let rc = notes("rc", "/perf");
        assert!(
            rc.starts_with("Release candidate 2 of Not-Everything 0.2.0, for testing.")
                && rc.ends_with(&format!("{entry}\n{table}")),
            "{rc}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// `perf_section` writes PERF_DIR's metrics and the disk image's size,
    /// runs the release build's fx-bench (a fake that records its
    /// arguments) and keeps its table and the values; a failing fx-bench
    /// stops the release.
    #[cfg(target_os = "macos")]
    #[test]
    fn perf_section_writes_the_metrics_and_the_table() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_path("perf-section");
        let (root, out, perf, target) = (
            dir.join("root"),
            dir.join("out"),
            dir.join("perf"),
            dir.join("target"),
        );
        for d in [&root, &out, &perf, &target.join("release")] {
            fs::create_dir_all(d).unwrap();
        }
        let dmg = dir.join("Not-Everything-Preview-0.2.0-RC-1-macos-arm64.dmg");
        fs::write(&dmg, vec![0u8; 12345]).unwrap();
        // No final newline, as a hand-edited file may have.
        fs::write(perf.join("metrics.txt"), "startup_ms=210\ndmg_bytes=999").unwrap();
        let fake = target.join("release/fx-bench");
        // perf-compare (the gate, with the DMG size) records its arguments
        // apart and exits with FAKE_GATE_EXIT; perf-release with FAKE_EXIT.
        fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = perf-compare ]; then printf '%s\\n' \"$@\" >\"$FAKE_GATE_ARGS\"; \
             echo 'perf-compare: 30 pass, 0 fail, 0 not measured'; exit \"${FAKE_GATE_EXIT:-0}\"; fi\n\
             printf '%s\\n' \"$@\" >\"$FAKE_ARGS\"\n\
             [ \"${FAKE_EXIT:-0}\" = 0 ] || { echo 'fx-bench: perf-release: bad input' >&2; exit \"$FAKE_EXIT\"; }\n\
             printf '## Performance\\n\\n| Metric | 0.2.0-RC-1 |\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let args_file = dir.join("args");
        let gate_args_file = dir.join("gate-args");
        let body = format!(
            "PERF_DIR='{}'\nDMG='{}'\nOUT='{}'\nTARGET='{}'\nROOT='{}'\nPREV=0.1.3\nLABEL=0.2.0-RC-1\nperf_section",
            perf.display(),
            dmg.display(),
            out.display(),
            target.display(),
            root.display()
        );
        let run_gated = |exit: &str, gate_exit: &str| {
            run_functions(
                &["perf_section"],
                &body,
                &[
                    ("FAKE_ARGS", args_file.to_str().unwrap()),
                    ("FAKE_EXIT", exit),
                    ("FAKE_GATE_ARGS", gate_args_file.to_str().unwrap()),
                    ("FAKE_GATE_EXIT", gate_exit),
                ],
            )
        };
        let run = |exit: &str| run_gated(exit, "0");
        let read = |p: PathBuf| fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p:?}: {e}"));
        let metrics = out.join("perf-metrics.txt").display().to_string();
        let expected_args = |notes: bool| {
            let mut v = vec![
                "perf-release".to_owned(),
                "--baseline".into(),
                root.join("docs/gates/perf-baseline.toml")
                    .display()
                    .to_string(),
                "--previous".into(),
                root.join("docs/gates/releases/0.1.3.txt")
                    .display()
                    .to_string(),
                "--previous-label".into(),
                "0.1.3".into(),
                "--label".into(),
                "0.2.0-RC-1".into(),
            ];
            if notes {
                v.push("--notes".into());
                v.push(perf.join("notes.md").display().to_string());
            }
            v.push(metrics.clone());
            v.join("\n") + "\n"
        };

        let done = run("0");
        assert_eq!(done.status.code(), Some(0), "{}", describe(&done));
        let values = "startup_ms=210\ndmg_bytes=999\ndmg_bytes=12345\n";
        assert_eq!(read(out.join("perf-metrics.txt")), values);
        assert_eq!(read(out.join("metrics-0.2.0-RC-1.txt")), values);
        assert_eq!(read(args_file.clone()), expected_args(false));
        // The whole rc tier is judged again with the DMG size.
        assert_eq!(
            read(gate_args_file.clone()),
            format!(
                "perf-compare\n--baseline\n{}\n--tier\nrc\n{metrics}\n",
                root.join("docs/gates/perf-baseline.toml").display()
            )
        );
        assert!(read(out.join("perf-gate.md")).contains("0 fail, 0 not measured"));
        let table = "## Performance\n\n| Metric | 0.2.0-RC-1 |\n";
        assert_eq!(read(out.join("performance.md")), table);
        assert!(String::from_utf8_lossy(&done.stdout).contains(table));

        // The owner's notes go to perf-release when the run has them.
        fs::write(perf.join("notes.md"), "Measured on a quiet Mac.\n").unwrap();
        let done = run("0");
        assert_eq!(done.status.code(), Some(0), "{}", describe(&done));
        assert_eq!(read(args_file.clone()), expected_args(true));

        // A metric that fails, or isn't measured, once the DMG size is in
        // stops the release before the table is made.
        fs::remove_file(&args_file).unwrap();
        let gated = run_gated("0", "1");
        assert_refused(
            &gated,
            1,
            "don't pass the gate",
            "a release whose metrics fail with the DMG size",
        );
        assert!(!args_file.exists(), "no table is made for a failing gate");

        let failed = run("2");
        assert_refused(
            &failed,
            1,
            "perf-release",
            "a failing fx-bench perf-release",
        );
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("bad input"),
            "{}",
            describe(&failed)
        );
        fs::remove_dir_all(&dir).ok();
    }
}
