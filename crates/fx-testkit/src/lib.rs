//! Workspace policy tests and test helpers.
//!
//! The tests in `tests/` check the repository against the project's rules,
//! among them every crate's name, licence, publish setting and
//! dependencies, the privacy and public-repository rules, the third-party
//! notices and asset attribution, and the release and gate scripts and the
//! CI workflow.
//!
//! The library holds helpers other crates' tests use as a dev-dependency:
//! [`crash`] runs a test binary's own test as a child process and kills it
//! with SIGKILL, for the crash-safety tests; `disk_image` (macOS only)
//! makes and attaches small disk images with `hdiutil` for the
//! external-volume tests. Std only.

pub mod crash;
#[cfg(target_os = "macos")]
pub mod disk_image;
