//! Small disk images for the external-volume tests (run 2.4), made and
//! attached with `hdiutil` (macOS only).
//!
//! [`DiskImage::attach_new`] creates an image of one [`ImageFs`] in a
//! scratch folder and attaches it with `-nobrowse` (so the Finder never
//! shows it and nothing else indexes it) at a mount point the test names.
//! [`DiskImage::detach`] detaches it; dropping a still-attached image
//! detaches it with `-force`, so a failing test never leaves a volume
//! mounted.
//!
//! Where `hdiutil` cannot make or attach images (a runner without disk
//! image support, a sandbox), [`DiskImage::attach_new`] fails with a
//! message, and the tests skip: [`skip`] prints why.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        Err(io::Error::other(format!(
            "{:?} failed: {}",
            cmd.get_args().collect::<Vec<_>>(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
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

    /// Detaches the image (a no-op when it is not attached).
    ///
    /// # Errors
    ///
    /// `hdiutil detach` failed (the image stays marked attached, so drop
    /// retries with `-force`).
    pub fn detach(&mut self) -> io::Result<()> {
        if !self.attached {
            return Ok(());
        }
        run(Command::new("hdiutil")
            .args(["detach", "-quiet"])
            .arg(&self.mount_point))?;
        self.attached = false;
        Ok(())
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
