//! Child processes for crash tests.
//!
//! A crash test runs its own test binary again as a child process that runs
//! one test, reads the one-line reports that child prints with [`report`],
//! and ends it with SIGKILL, as `kill -9` does ([`Child::kill9`]). The test
//! the child runs is marked `#[ignore]`, so a normal run skips it, and does
//! its work only when [`is_child`] says so, so `cargo test -- --ignored`
//! passes it at once:
//!
//! ```ignore
//! #[test]
//! #[ignore = "the child process of the crash tests"]
//! fn crash_child() {
//!     if !fx_testkit::crash::is_child() {
//!         return;
//!     }
//!     fx_testkit::crash::report("ready");
//!     // ... work until the parent kills this process.
//! }
//! ```
//!
//! The parent finds the child through the test's name in the binary (what
//! `cargo test -- --list` prints) and passes it what it needs in
//! environment variables.
//!
//! Std only: `std::process::Child::kill` sends SIGKILL on Unix.

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{ChildStderr, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Set in the environment of every child [`Child::spawn`] starts.
pub const CHILD_ENV: &str = "FX_TESTKIT_CRASH_CHILD";

/// Starts each line [`report`] prints, so the parent can tell the reports
/// from the test harness's own output on the same stream.
const TAG: &str = "fx-testkit-report: ";

/// How many bytes of the child's stderr an error keeps (the end of it,
/// where a panic message is).
const STDERR_TAIL: usize = 4096;

/// Whether this process is a child started by [`Child::spawn`].
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// Prints `message` as one report for the parent's
/// [`Child::next_report`], and flushes it at once, so a report printed
/// before a kill always reaches the parent.
///
/// # Panics
///
/// Panics if `message` holds a line break: a report is one line.
pub fn report(message: impl fmt::Display) {
    let message = message.to_string();
    assert!(
        !message.contains(['\n', '\r']),
        "a report is one line: {message:?}"
    );
    let mut out = io::stdout().lock();
    // The harness may have left `test name ... ` on the current line, so
    // the parent looks for the tag anywhere in a line.
    let _ = writeln!(out, "{TAG}{message}");
    let _ = out.flush();
}

/// Why [`Child::next_report`] returned no report.
#[derive(Debug)]
pub enum ChildError {
    /// No report came within the timeout; the child is still running.
    Timeout(Duration),
    /// The child exited (or closed its output) without another report.
    Exited {
        /// How it exited.
        status: io::Result<ExitStatus>,
        /// The end of what it printed on stderr, such as a panic message.
        stderr: String,
    },
}

impl fmt::Display for ChildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChildError::Timeout(t) => write!(f, "no report from the child within {t:?}"),
            ChildError::Exited { status, stderr } => {
                match status {
                    Ok(s) => write!(f, "the child exited ({s})")?,
                    Err(e) => write!(f, "the child exited (status unknown: {e})")?,
                }
                if stderr.trim().is_empty() {
                    Ok(())
                } else {
                    write!(f, "; its stderr ends with:\n{stderr}")
                }
            }
        }
    }
}

impl std::error::Error for ChildError {}

/// A child killed by [`Child::kill9`].
#[derive(Debug)]
pub struct Killed {
    /// The exit status: killed by SIGKILL, unless the child had already
    /// exited.
    pub status: ExitStatus,
    /// Every report the child printed that [`Child::next_report`] had not
    /// returned yet, in order.
    pub reports: Vec<String>,
}

/// A running child test process. Dropping it kills the child and waits for
/// it, so a failing parent never leaves one behind.
pub struct Child {
    process: std::process::Child,
    reports: Receiver<String>,
    stdout: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

impl Child {
    /// Starts the current test binary as a child process that runs only
    /// the ignored test `test` (its full name in the binary), with
    /// [`CHILD_ENV`] and `envs` set in its environment.
    ///
    /// # Errors
    ///
    /// Finding the current executable or starting the process failed.
    pub fn spawn<I, K, V>(test: &str, envs: I) -> io::Result<Child>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let mut process = Command::new(std::env::current_exe()?)
            .args([
                test,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
                "--quiet",
            ])
            .env(CHILD_ENV, "1")
            .envs(envs)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = process.stdout.take().expect("stdout is piped");
        let stderr = process.stderr.take().expect("stderr is piped");
        let (tx, reports) = mpsc::channel();
        let stdout = thread::Builder::new()
            .name("fx-testkit-child-stdout".into())
            .spawn(move || {
                // Reads to the end, so the child never blocks on a full pipe.
                for line in BufReader::new(stdout).split(b'\n') {
                    let Ok(line) = line else { break };
                    let line = String::from_utf8_lossy(&line);
                    if let Some(at) = line.find(TAG) {
                        let _ = tx.send(line[at + TAG.len()..].trim_end().to_string());
                    }
                }
            })?;
        let stderr = thread::Builder::new()
            .name("fx-testkit-child-stderr".into())
            .spawn(move || read_tail(stderr))?;
        Ok(Child {
            process,
            reports,
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    /// The child's process ID.
    pub fn id(&self) -> u32 {
        self.process.id()
    }

    /// The child's next report, waiting at most `timeout` for it.
    ///
    /// # Errors
    ///
    /// [`ChildError::Timeout`] when none came in time (the child keeps
    /// running), [`ChildError::Exited`] when the child exited first.
    pub fn next_report(&mut self, timeout: Duration) -> Result<String, ChildError> {
        match self.reports.recv_timeout(timeout) {
            Ok(report) => Ok(report),
            Err(RecvTimeoutError::Timeout) => Err(ChildError::Timeout(timeout)),
            Err(RecvTimeoutError::Disconnected) => {
                let status = self.process.wait();
                Err(ChildError::Exited {
                    status,
                    stderr: self.stderr_tail(),
                })
            }
        }
    }

    /// Kills the child with SIGKILL (as `kill -9` does), waits for it, so
    /// its process ID names no process any more, and returns every report
    /// it printed before it died that was not read yet.
    ///
    /// # Errors
    ///
    /// Sending the signal or waiting failed.
    pub fn kill9(mut self) -> io::Result<Killed> {
        // An error here means the child has already exited: `wait` still
        // reaps it and reports how.
        let _ = self.process.kill();
        let status = self.process.wait()?;
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        let reports = self.reports.try_iter().collect();
        Ok(Killed { status, reports })
    }

    fn stderr_tail(&mut self) -> String {
        let bytes = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Reads `stderr` to the end and keeps its last [`STDERR_TAIL`] bytes.
fn read_tail(mut stderr: ChildStderr) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) | Err(_) => return tail,
            Ok(n) => {
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > 2 * STDERR_TAIL {
                    tail.drain(..tail.len() - STDERR_TAIL);
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::time::Instant;

    use super::*;

    const WAIT: Duration = Duration::from_secs(20);

    /// The child of the tests below: reports what its environment asks
    /// for, then waits to be killed or exits.
    #[test]
    #[ignore = "the child process of the crash helper's tests"]
    fn child_test() {
        if !is_child() {
            return;
        }
        let word = std::env::var("FX_TESTKIT_TEST_WORD").unwrap_or_default();
        report(format!("hello {word}"));
        report("second");
        match std::env::var("FX_TESTKIT_TEST_END").as_deref() {
            Ok("panic") => panic!("the child gives up"),
            Ok("exit") => {}
            _ => loop {
                thread::sleep(Duration::from_secs(60));
            },
        }
    }

    fn spawn(end: &str) -> Child {
        Child::spawn(
            "crash::tests::child_test",
            [
                ("FX_TESTKIT_TEST_WORD", "there"),
                ("FX_TESTKIT_TEST_END", end),
            ],
        )
        .expect("spawn the child")
    }

    #[test]
    fn reports_arrive_in_order_and_a_kill_is_sigkill() {
        let mut child = spawn("wait");
        assert_eq!(child.next_report(WAIT).expect("first"), "hello there");
        let pid = child.id();
        let start = Instant::now();
        // The second report may already be in the pipe: the kill hands it
        // back, or it was read here.
        let killed = child.kill9().expect("kill");
        assert!(start.elapsed() < WAIT);
        assert_eq!(killed.status.signal(), Some(9), "{:?}", killed.status);
        assert!(killed.reports.len() <= 1, "{:?}", killed.reports);
        assert!(pid > 0);
    }

    #[test]
    fn a_child_that_is_still_running_times_out() {
        let mut child = spawn("wait");
        child.next_report(WAIT).expect("first");
        child.next_report(WAIT).expect("second");
        match child.next_report(Duration::from_millis(50)) {
            Err(ChildError::Timeout(_)) => {}
            other => panic!("expected a timeout, got {other:?}"),
        }
        child.kill9().expect("kill");
    }

    #[test]
    fn a_child_that_exits_is_reported_with_its_stderr() {
        let mut child = spawn("panic");
        child.next_report(WAIT).expect("first");
        child.next_report(WAIT).expect("second");
        match child.next_report(WAIT) {
            Err(ChildError::Exited { status, stderr }) => {
                assert!(!status.expect("status").success());
                assert!(stderr.contains("the child gives up"), "{stderr}");
            }
            other => panic!("expected an exit, got {other:?}"),
        }
    }

    #[test]
    fn a_child_that_ends_normally_reports_its_exit() {
        let mut child = spawn("exit");
        child.next_report(WAIT).expect("first");
        child.next_report(WAIT).expect("second");
        match child.next_report(WAIT) {
            Err(ChildError::Exited { status, .. }) => {
                assert!(status.expect("status").success());
            }
            other => panic!("expected an exit, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "a report is one line")]
    fn a_report_is_one_line() {
        report("two\nlines");
    }
}
