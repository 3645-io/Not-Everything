//! Helpers for the tests of the shell scripts in `scripts/`: reading a
//! script's code without its comments and messages, and running a copy of a
//! script in a throwaway git repository, so its git checks can be tested
//! without touching this checkout. Only `std` is used.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::repo_root;

// ---------------------------------------------------------------------------
// Reading a script.
// ---------------------------------------------------------------------------

/// The script's executable lines: comment lines and here-document bodies
/// (usage text) removed, and trailing `#` comments stripped.
pub fn code_lines(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut heredoc_end: Option<String> = None;
    for raw in script.lines() {
        if let Some(end) = &heredoc_end {
            if raw.trim() == end {
                heredoc_end = None;
            }
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line = strip_shell_comment(raw);
        if let Some(end) = heredoc_terminator(&line) {
            heredoc_end = Some(end);
        }
        out.push(line);
    }
    out
}

/// Removes a trailing ` # comment` outside quotes.
pub fn strip_shell_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return line[..i].to_owned(),
            None => {}
        }
        prev = c;
    }
    line.to_owned()
}

/// If `line` starts a here-document (`<<EOF`, `<<-EOF`, `<<'EOF'`,
/// `<<"EOF"`), returns its terminator word.
pub fn heredoc_terminator(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let rest = &line[idx + 2..];
    if rest.starts_with('<') {
        return None; // `<<<` here-string
    }
    let rest = rest.trim_start_matches('-').trim_start();
    let word: String = rest
        .chars()
        .filter(|c| *c != '\'' && *c != '"')
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Removes the contents of single- and double-quoted strings, so that
/// messages are not mistaken for commands.
pub fn strip_quoted(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => {
                quote = None;
                out.push(c);
            }
            Some(_) => {}
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push(c);
            }
            None => out.push(c),
        }
    }
    out
}

/// Shell words of unquoted code, split on whitespace and operators.
pub fn words(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || ";|&()`{}".contains(c))
        .filter(|w| !w.is_empty())
        .map(|w| w.trim_start_matches('$').to_owned())
        .collect()
}

/// The line number (0-based, among `code`) of the first line containing
/// `needle`.
pub fn first_line_with(code: &[String], needle: &str) -> Option<usize> {
    code.iter().position(|l| l.contains(needle))
}

/// `code` (from [`code_lines`]) with each line ending in `\` joined to the
/// next, as the shell reads it: each command with the index of its first
/// line in `code`.
pub fn logical_lines(code: &[String]) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for (i, line) in code.iter().enumerate() {
        let trimmed = line.trim_end();
        let (text, continues) = match trimmed.strip_suffix('\\') {
            Some(head) => (head.trim_end(), true),
            None => (trimmed, false),
        };
        match out.last_mut() {
            Some((_, joined)) if open => {
                joined.push(' ');
                joined.push_str(text.trim_start());
            }
            _ => out.push((i, text.to_owned())),
        }
        open = continues;
    }
    out
}

/// The text of the top-level shell function `name` in `script`: from its
/// unindented `name() {` line to the first line that is just `}`, or that
/// one line for a one-line function. `None` if there is no such function.
pub fn shell_function(script: &str, name: &str) -> Option<String> {
    let head = format!("{name}() {{");
    let mut lines = script.lines().skip_while(|l| !l.starts_with(&head));
    let first = lines.next()?;
    if first.trim_end().ends_with('}') {
        return Some(first.to_owned());
    }
    let mut out = vec![first];
    for line in lines {
        out.push(line);
        if line == "}" {
            return Some(out.join("\n"));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Running a script.
// ---------------------------------------------------------------------------

/// The system folders only: git, sed and the other tools a script needs, but
/// not cargo, so a script that gets past its checks stops at "cargo is not
/// on PATH" instead of building.
pub const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Runs `cmd` and returns its output. A script under test must refuse or
/// stop before any build, signing or upload, so a run that takes longer than
/// 30 s is killed and fails the test.
pub fn run_with_timeout(cmd: &mut Command) -> Output {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"));
    let start = Instant::now();
    loop {
        if child.try_wait().expect("wait for the script").is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{cmd:?} did not exit within 30 s; it must refuse before building, \
                 signing or contacting Apple or GitHub"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child
        .wait_with_output()
        .expect("collect the script's output")
}

/// The exit status and both streams, for assertion messages.
pub fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Asserts that the script exited with `code` and said `phrase` on stderr.
pub fn assert_refused(out: &Output, code: i32, phrase: &str, why: &str) {
    assert_eq!(out.status.code(), Some(code), "{why}\n{}", describe(out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(phrase),
        "{why}: stderr must say {phrase:?}\n{}",
        describe(out)
    );
}

/// A fresh, unused path under the system temp directory.
pub fn temp_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "fx-testkit-{tag}-{}-{}-{nanos}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Every path under `dir`, relative to it, sorted.
pub fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in fs::read_dir(&p).expect("read the folder") {
            let path = entry.expect("folder entry").path();
            out.push(
                path.strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A throwaway git repository under the temp directory, removed on drop.
/// Git runs without the user's or the system's configuration, so signing
/// settings or hooks there never change what a test sees.
pub struct Sandbox {
    /// The repository's working tree.
    pub dir: PathBuf,
}

impl Sandbox {
    /// An empty repository whose first branch is `main`.
    pub fn new(tag: &str) -> Sandbox {
        let dir = temp_path(tag);
        fs::create_dir_all(&dir).unwrap();
        let sandbox = Sandbox { dir };
        sandbox.git(&["init", "--quiet"]);
        sandbox.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        sandbox
    }

    /// The environment every command in the sandbox runs with.
    fn isolate(&self, cmd: &mut Command) {
        cmd.current_dir(&self.dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
    }

    /// Runs git with `args` and returns its trimmed standard output; panics
    /// if git fails.
    pub fn git(&self, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args);
        self.isolate(&mut cmd);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("cannot run git {args:?}: {e}"));
        assert!(out.status.success(), "git {args:?}\n{}", describe(&out));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Writes `text` to `rel` in the working tree, creating folders.
    pub fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Copies `rel` from this checkout into the sandbox at the same path.
    pub fn copy_from_repo(&self, rel: &str) {
        let text = fs::read_to_string(repo_root().join(rel))
            .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"));
        self.write(rel, &text);
    }

    /// Commits everything in the working tree and returns the commit.
    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Runs the sandbox's copy of the script `rel` with `args` under `sh`,
    /// with only the system folders on `PATH`.
    pub fn run(&self, rel: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new("sh");
        cmd.arg(self.dir.join(rel))
            .args(args)
            .env("PATH", SYSTEM_PATH)
            .env_remove("NE_CHANNEL")
            .env_remove("NE_CHANNEL_NUMBER")
            .env_remove("NE_RELEASE_REPO");
        self.isolate(&mut cmd);
        run_with_timeout(&mut cmd)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
