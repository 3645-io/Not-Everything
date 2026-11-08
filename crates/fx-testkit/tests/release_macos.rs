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
//!   release candidates and betas after the highest existing tag.
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
    let build = first_line_with(&code, "build -p fx-ui-macos -p fx-cli --bins")
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
        "scripts/release-macos.sh [--rc | --beta] [--publish] NEW_OUT_DIR",
        "scripts/release-macos.sh --resume OUT_DIR [--publish]",
    ] {
        assert!(text.contains(usage), "{SCRIPT} usage must show {usage:?}");
    }
    let code = code_lines(&text).join("\n");
    for option in ["--resume", "--rc", "--beta", "--publish"] {
        assert!(
            code.contains(option),
            "{SCRIPT} must handle the `{option}` option"
        );
    }
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
}
