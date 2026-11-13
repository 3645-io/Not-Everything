//! The disk images of the external-volume tests (`fx_testkit::disk_image`,
//! macOS only).
//!
//! Contract pinned here:
//! - `DiskImage::detach` waits for a volume that is busy for a moment (a
//!   file still open on it) and detaches it once it is released. On
//!   GitHub's runners a test's image was now and then still busy when the
//!   test detached it (12–13 Nov), and a single try failed the test.
//! - A volume still busy after `DETACH_PATIENCE` fails the detach with the
//!   last error, which says the volume is busy and which of the test's own
//!   processes holds it, so a test that keeps a volume busy (or an app
//!   that keeps a user from ejecting a disk) is not hidden.
//! - Any other failure fails at once.
//!
//! Where `hdiutil` cannot make or attach images, the tests skip.

#![cfg(target_os = "macos")]

use std::fs::File;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use fx_testkit::disk_image::{DETACH_PATIENCE, DiskImage, ImageFs, Scratch, skip};

/// An APFS image attached at `<scratch>/mnt`, or `None` (the test skips).
fn attach(scratch: &Scratch, test: &str) -> Option<DiskImage> {
    let mp = scratch.path().join("mnt");
    match DiskImage::attach_new(scratch.path(), "img", ImageFs::Apfs, &mp) {
        Ok(img) => Some(img),
        Err(e) => {
            skip(test, &e);
            None
        }
    }
}

#[test]
fn a_detach_waits_for_a_busy_volume_to_be_released() {
    let scratch = Scratch::new("detach-busy").expect("scratch");
    let Some(mut img) = attach(&scratch, "a_detach_waits_for_a_busy_volume") else {
        return;
    };
    let held = File::create(img.mount_point().join("held.txt")).expect("a file on the image");
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_secs(1));
        drop(held);
    });
    let started = Instant::now();
    let result = img.detach();
    release.join().expect("the releasing thread");
    result.expect("the detach waits for the file to be closed");
    assert!(!img.is_attached());
    // The thread's second started just before `started`.
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "detached while the file was open"
    );
}

#[test]
fn a_volume_busy_past_the_patience_fails_the_detach_naming_the_holder() {
    let scratch = Scratch::new("detach-stuck").expect("scratch");
    let Some(mut img) = attach(&scratch, "a_volume_busy_past_the_patience") else {
        return;
    };
    let held = File::create(img.mount_point().join("held.txt")).expect("a file on the image");
    let started = Instant::now();
    let err = img
        .detach()
        .expect_err("a volume kept busy can't be detached");
    let waited = started.elapsed();
    assert!(
        waited >= DETACH_PATIENCE,
        "gave up after {waited:?}, before {DETACH_PATIENCE:?}"
    );
    assert!(
        waited < DETACH_PATIENCE + Duration::from_secs(10),
        "kept trying for {waited:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("busy"), "says the volume is busy: {msg}");
    assert!(
        msg.contains(&format!("[{}]", std::process::id())),
        "names this process as the holder: {msg}"
    );
    assert!(img.is_attached(), "still attached after a failed detach");
    drop(held);
    img.detach().expect("detaches once the file is closed");
}

#[test]
fn a_detach_that_fails_for_another_reason_fails_at_once() {
    let scratch = Scratch::new("detach-other").expect("scratch");
    let Some(mut img) = attach(&scratch, "a_detach_that_fails_for_another_reason") else {
        return;
    };
    // Detached behind the helper's back: its own detach then fails because
    // nothing is mounted there any more, not because anything is busy.
    let out = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(img.mount_point())
        .output()
        .expect("hdiutil runs");
    assert!(out.status.success(), "the image detaches: {out:?}");
    let started = Instant::now();
    img.detach()
        .expect_err("nothing is mounted at the mount point");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "retried a failure that wasn't a busy volume for {:?}",
        started.elapsed()
    );
}
