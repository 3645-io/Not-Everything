//! Small disk images for the external-volume tests (run 2.4), made and
//! attached with `hdiutil` (macOS only).
//!
//! [`DiskImage::attach_new`] creates an image of one [`ImageFs`] in a
//! scratch folder and attaches it with `-nobrowse` (so the Finder never
//! shows it and nothing else indexes it) at a mount point the test names.
//! [`DiskImage::detach`] detaches it, and waits up to [`DETACH_PATIENCE`]
//! while the volume is busy: on GitHub's runners a test's image was now and
//! then still busy when the test detached it (12–13 Nov). Dropping a
//! still-attached image detaches it with `-force`, so a failing test never
//! leaves a volume mounted.
//!
//! Where `hdiutil` cannot make or attach images (a runner without disk
//! image support, a sandbox), [`DiskImage::attach_new`] fails with a
//! message, and the tests skip: [`skip`] prints why.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

/// How long [`DiskImage::detach`] keeps trying while the volume is busy.
pub const DETACH_PATIENCE: Duration = Duration::from_secs(5);

/// The first pause between two tries of a busy detach; each pause doubles,
/// up to [`LONGEST_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(50);

/// The longest pause between two tries of a busy detach.
const LONGEST_PAUSE: Duration = Duration::from_millis(800);

/// `hdiutil detach`'s exit status when a file is still open on the volume
/// (EBUSY). With `-quiet` it prints nothing then; without, "Resource busy".
const EBUSY: i32 = 16;

/// The file systems the tests use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFs {
    /// APFS.
    Apfs,
    /// Mac OS Extended (HFS+).
    HfsPlus,
    /// MS-DOS FAT32.
    Fat32,
    /// exFAT.
    ExFat,
}

impl ImageFs {
    /// All four, in the order above.
    pub const ALL: [ImageFs; 4] = [
        ImageFs::Apfs,
        ImageFs::HfsPlus,
        ImageFs::Fat32,
        ImageFs::ExFat,
    ];

    /// `hdiutil create -fs` name.
    fn hdiutil_name(self) -> &'static str {
        match self {
            ImageFs::Apfs => "APFS",
            ImageFs::HfsPlus => "HFS+",
            ImageFs::Fat32 => "MS-DOS FAT32",
            ImageFs::ExFat => "ExFAT",
        }
    }

    /// The `statfs` type name macOS reports for a mounted volume.
    pub fn fs_type(self) -> &'static str {
        match self {
            ImageFs::Apfs => "apfs",
            ImageFs::HfsPlus => "hfs",
            ImageFs::Fat32 => "msdos",
            ImageFs::ExFat => "exfat",
        }
    }

    /// A short lowercase name for folders and volume names.
    pub fn short(self) -> &'static str {
        match self {
            ImageFs::Apfs => "apfs",
            ImageFs::HfsPlus => "hfs",
            ImageFs::Fat32 => "fat32",
            ImageFs::ExFat => "exfat",
        }
    }

    /// Image size: FAT32 needs at least about 33 MB.
    fn size(self) -> &'static str {
        match self {
            ImageFs::Fat32 => "40m",
            _ => "16m",
        }
    }

    /// Partition layout `hdiutil create` accepts for the type.
    fn layout(self) -> &'static str {
        match self {
            ImageFs::Fat32 => "MBRSPUD",
            _ => "GPTSPUD",
        }
    }
}

/// An attached disk image. See the module docs.
#[derive(Debug)]
pub struct DiskImage {
    image: PathBuf,
    mount_point: PathBuf,
    attached: bool,
}

fn run(cmd: &mut Command) -> io::Result<Output> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(failed(cmd, &out))
    }
}

/// The error for a failed command: its arguments, exit status and message
/// (empty for `hdiutil -quiet`).
fn failed(cmd: &Command, out: &Output) -> io::Error {
    io::Error::other(format!(
        "{:?} failed ({}): {}",
        cmd.get_args().collect::<Vec<_>>(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Whether a failed `hdiutil detach` found the volume busy.
fn busy(out: &Output) -> bool {
    out.status.code() == Some(EBUSY)
        || String::from_utf8_lossy(&out.stderr).contains("Resource busy")
}

/// The processes of this user with a file open on the volume mounted at
/// `mount_point`, as `command[pid]` (`lsof` sees only this user's), or why
/// none are named.
fn holders(mount_point: &Path) -> String {
    let out = match Command::new("lsof")
        .args(["-n", "-P", "-F", "pc", "+f", "--"])
        .arg(mount_point)
        .output()
    {
        Ok(out) => out,
        Err(e) => return format!("lsof failed: {e}"),
    };
    let mut names = Vec::new();
    let mut pid = "";
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p;
        } else if let Some(command) = line.strip_prefix('c') {
            names.push(format!("{command}[{pid}]"));
        }
    }
    if names.is_empty() {
        "none of this user's processes (a system service, or released since)".to_string()
    } else {
        names.join(", ")
    }
}

impl DiskImage {
    /// Creates an image of `fs` named `name.dmg` in `dir` and attaches it
    /// with `-nobrowse` at `mount_point` (created if missing; its parent
    /// must exist).
    ///
    /// # Errors
    ///
    /// `hdiutil` is missing or failed to create or attach the image.
    pub fn attach_new(
        dir: &Path,
        name: &str,
        fs: ImageFs,
        mount_point: &Path,
    ) -> io::Result<DiskImage> {
        let image = dir.join(format!("{name}.dmg"));
        run(Command::new("hdiutil")
            .args([
                "create",
                "-quiet",
                "-ov",
                "-size",
                fs.size(),
                "-fs",
                fs.hdiutil_name(),
                "-layout",
                fs.layout(),
                "-volname",
                &format!("NE{}", fs.short().to_uppercase()),
            ])
            .arg("-o")
            .arg(&image))?;
        std::fs::create_dir_all(mount_point)?;
        let mut img = DiskImage {
            image,
            mount_point: mount_point.to_path_buf(),
            attached: false,
        };
        img.attach()?;
        Ok(img)
    }

    /// Attaches the image again after [`detach`](Self::detach), at the same
    /// mount point (created if missing).
    ///
    /// # Errors
    ///
    /// `hdiutil attach` failed.
    pub fn attach(&mut self) -> io::Result<()> {
        if self.attached {
            return Ok(());
        }
        std::fs::create_dir_all(&self.mount_point)?;
        run(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-noverify", "-noautoopen"])
            .arg("-mountpoint")
            .arg(&self.mount_point)
            .arg(&self.image))?;
        self.attached = true;
        Ok(())
    }

    /// Detaches the image (a no-op when it is not attached). While the
    /// volume is busy (a file is open on it) it tries again, after pauses
    /// growing from 50 to 800 ms, for up to [`DETACH_PATIENCE`]; when it
    /// detached only after a retry, it says so on stderr, even in a test
    /// that passes, with what held the volume. It never forces the detach:
    /// a volume that stays busy is a test (or an app) keeping a disk from
    /// being ejected, and fails.
    ///
    /// # Errors
    ///
    /// `hdiutil detach` failed for another reason than a busy volume, at
    /// once, or the volume was still busy after [`DETACH_PATIENCE`]: the
    /// error gives the last try's exit status and message and, for a busy
    /// volume, this user's processes that held it. The image stays marked
    /// attached, so dropping it detaches it with `-force`.
    pub fn detach(&mut self) -> io::Result<()> {
        if !self.attached {
            return Ok(());
        }
        let started = Instant::now();
        let end = started + DETACH_PATIENCE;
        let mut pause = FIRST_PAUSE;
        let mut held_by = None;
        let mut tries = 0;
        loop {
            let mut cmd = Command::new("hdiutil");
            cmd.args(["detach", "-quiet"]).arg(&self.mount_point);
            let out = cmd.output()?;
            tries += 1;
            if out.status.success() {
                self.attached = false;
                if let Some(held_by) = held_by {
                    // Written to stderr itself, past the test harness's
                    // capture (which takes only `eprintln!`), so a CI log
                    // shows it even when the test passes: whatever keeps a
                    // volume busy is worth seeing.
                    let _ = writeln!(
                        io::stderr(),
                        "{}: busy, detached at try {tries} after {:?}; held by {held_by}",
                        self.mount_point.display(),
                        started.elapsed()
                    );
                }
                return Ok(());
            }
            if !busy(&out) {
                return Err(failed(&cmd, &out));
            }
            // Who holds it, looked up at the first busy try and the last:
            // whatever held it at first may be gone by then.
            let now = Instant::now();
            if held_by.is_none() || now >= end {
                held_by = Some(holders(&self.mount_point));
            }
            if now >= end {
                let err = failed(&cmd, &out);
                return Err(io::Error::other(format!(
                    "{err}; the volume was still busy after {tries} tries in {:?}, held by {}",
                    started.elapsed(),
                    held_by.unwrap_or_default()
                )));
            }
            thread::sleep(pause.min(end - now));
            pause = (pause * 2).min(LONGEST_PAUSE);
        }
    }

    /// Where the image is mounted.
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    /// Whether the image is attached.
    pub fn is_attached(&self) -> bool {
        self.attached
    }
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        if self.attached {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount_point)
                .output();
        }
        let _ = std::fs::remove_file(&self.image);
    }
}

/// A fresh scratch folder for one test, canonical (no symlinks in its
/// path, as mount points are reported), removed on drop.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Creates `<temp>/fx-volumes-<tag>-<pid>-<n>`.
    ///
    /// # Errors
    ///
    /// The folder could not be created.
    pub fn new(tag: &str) -> io::Result<Scratch> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("fx-volumes-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        let path = std::fs::canonicalize(&path)?;
        Ok(Scratch { path })
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Prints why a disk-image test is skipped (on stderr, shown with
/// `--nocapture` and in a failing run's output).
pub fn skip(test: &str, why: &dyn std::fmt::Display) {
    eprintln!("{test}: skipped, no disk images here: {why}");
}
