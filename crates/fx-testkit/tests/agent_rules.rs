//! Agent instruction tests. `.agents/CLAUDE.md` is a short index of the rule
//! files in `.agents/rules/`: it names every one, names none that is
//! missing, and imports the privacy and clean-room rules so that they always
//! load. The root `CLAUDE.md` imports the index and `AGENTS.md` points to it
//! for other tools.
//!
//! The shared `.claude/settings.json` holds nothing but permission lists.
//! It lets an agent run, without asking, only commands that change nothing
//! and run no code from the checked-out branch, which may be a stranger's
//! pull request ([`SAFE_ALLOW_RULES`]). It denies git's `--output`, which
//! writes a file from those read commands ([`OUTPUT_DENY`]); a quoted or
//! split form gets past that rule, and the conventions rule says so
//! ([`DODGES_OUTPUT_DENY`]). It keeps the clean-room deny rule. Build and
//! test commands, and commands that change the repository, are allowed
//! only in a developer's own `.claude/settings.local.json` (gitignored),
//! never in the shared file.
//!
//! The rules describe the public repository, and the licensing rule
//! installs the cargo-about version the notices script needs. Only `std` is
//! used.

mod common;

use common::{read, repo_root};
use std::fs;

const INDEX: &str = ".agents/CLAUDE.md";
const RULES: &str = ".agents/rules";
const ROOT_CLAUDE: &str = "CLAUDE.md";
const AGENTS: &str = "AGENTS.md";
const SETTINGS: &str = ".claude/settings.json";
const BRANCHES: &str = ".agents/rules/branches-and-releases.md";
const LICENSING: &str = ".agents/rules/licensing.md";
const CONVENTIONS: &str = ".agents/rules/conventions.md";
const NOTICES_SCRIPT: &str = "scripts/third-party-notices.sh";

/// Where the project lives.
const REPOSITORY: &str = "3645-io/Not-Everything";

/// The rule files the index imports, so they load in every session.
const ALWAYS_LOADED: [&str; 2] = ["privacy", "clean-room"];

/// The only rules the shared settings may allow: git's read commands. None
/// writes a file or runs anything the checked-out branch holds, once
/// [`OUTPUT_DENY`] takes out git's `--output`.
const SAFE_ALLOW_RULES: [&str; 6] = [
    "Bash(git status *)",
    "Bash(git diff *)",
    "Bash(git log *)",
    "Bash(git show *)",
    "Bash(git branch --show-current)",
    "Bash(git worktree list *)",
];

/// The deny rule for git's `--output=<file>` (or `--output <file>`), with
/// which `git diff`, `git log` and `git show` write a file. git refuses
/// shortened forms such as `--outp`, so this one rule covers them all.
const OUTPUT_DENY: &str = "Bash(git * --output*)";

/// Commands the safe rules allow that write a file through `--output`.
const WRITES_THROUGH_OUTPUT: [&str; 5] = [
    "git diff --output=x",
    "git diff --output x",
    "git log -p --output=x",
    "git show --output=x HEAD",
    "git diff HEAD~1 --output=x -- src",
];

/// Forms of `--output` that get past [`OUTPUT_DENY`]. A rule matches the
/// command as written, so quoting or splitting the option dodges it, and
/// git still writes the file. The conventions rule says so
/// ([`QUOTING_LIMIT`]); these record the limit, so a change to it is seen.
const DODGES_OUTPUT_DENY: [&str; 3] = [
    "git log -1 \"--output=x\"",
    "git show --outpu''t=x HEAD",
    "git diff '--output' x",
];

/// What the conventions rule says about [`DODGES_OUTPUT_DENY`].
const QUOTING_LIMIT: &str = "a quoted or split `--output`";

/// Commands the shared settings must never allow: they change the
/// repository, its branches or its tags, or run code from the checked-out
/// branch. Building, testing, linting, documenting or benching runs the
/// branch's build scripts, proc macros and tests. Even `cargo fmt --check`,
/// `cargo metadata` and `cargo tree` run a program the branch's
/// `.cargo/config.toml` names: an alias that shadows `fmt`, or the
/// compiler or wrapper that cargo asks for its version. Through rustup,
/// `rustc --version` and `rustup show` run the toolchain the branch's
/// `rust-toolchain.toml` names, which may be a program at any path.
const NOT_ALLOWED: [&str; 33] = [
    "git add .",
    "git commit -m x",
    "git switch dev",
    "git switch -c topic",
    "git checkout dev",
    "git checkout -b topic",
    "git branch topic",
    "git branch -D dev",
    "git branch -m topic",
    "git push",
    "git push origin dev",
    "git tag release/v1.0.0",
    "git stash",
    "git reset --hard",
    "git worktree add ../x",
    "cargo fmt",
    "cargo fmt --all",
    "cargo fmt --check",
    "cargo fmt --all --check",
    "cargo metadata --format-version 1",
    "cargo tree",
    "cargo build",
    "cargo check",
    "cargo test",
    "cargo test --workspace",
    "cargo clippy",
    "cargo doc",
    "cargo bench",
    "cargo run",
    "cargo install cargo-about",
    "sh scripts/third-party-notices.sh",
    "rustc --version",
    "rustup show",
];

/// The only keys the shared settings may hold. Anything else (hooks, a
/// default permission mode, an API key helper, environment variables) can
/// run code or skip the question.
const SETTINGS_KEYS: [&str; 5] = ["$schema", "permissions", "allow", "ask", "deny"];

/// Wording the rules may no longer use, any case: the repository is
/// public, and a personal noreply address names a personal account (the
/// rules say "a GitHub noreply address").
const PRE_PUBLIC: [&str; 6] = [
    "meant to be public",
    "will be `3645-io",
    "until the owner publishes",
    "repository is private",
    "metered",
    "users.noreply.github.com",
];

/// The clean-room rule: the local summary of FSearch's GPL internals is never
/// read.
const RESEARCH_DENY: &str = "Read(**/search_engine_index_design.md)";

/// The rule names (file stems) of the `.md` files in `.agents/rules/`.
fn rule_files() -> Vec<String> {
    let dir = repo_root().join(RULES);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter_map(|n| n.strip_suffix(".md").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// The rule names `text` mentions as `rules/<name>.md`.
fn named_rules(text: &str) -> Vec<String> {
    let mut names: Vec<String> = text
        .match_indices("rules/")
        .filter_map(|(i, m)| {
            let rest = &text[i + m.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            let (name, after) = rest.split_at(end);
            (!name.is_empty() && after.starts_with(".md")).then(|| name.to_owned())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The lines of a Markdown file outside fenced code blocks, where Claude
/// Code reads `@path` imports.
fn lines_outside_fences(text: &str) -> Vec<&str> {
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            out.push(line);
        }
    }
    out
}

/// Whether `text` imports `path` on a line of its own.
fn imports(text: &str, path: &str) -> bool {
    let import = format!("@{path}");
    lines_outside_fences(text)
        .iter()
        .any(|l| l.trim() == import)
}

/// The strings of the JSON array under `key` (the first such key), or
/// `None` when there is none. Enough JSON for a settings file: the array's
/// items are strings with backslash escapes.
fn string_array(json: &str, key: &str) -> Option<Vec<String>> {
    let at = json.find(&format!("\"{key}\""))?;
    let rest = &json[at + key.len() + 2..];
    let open = rest.find('[')?;
    let mut items = Vec::new();
    let mut chars = rest[open + 1..].chars();
    while let Some(c) = chars.next() {
        match c {
            ']' => return Some(items),
            '"' => {
                let mut item = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => item.extend(chars.next()),
                        '"' => break,
                        c => item.push(c),
                    }
                }
                items.push(item);
            }
            _ => {}
        }
    }
    None
}

/// Every key of every object in `json`: each string followed by a colon.
fn json_keys(json: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut item = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => item.extend(chars.next()),
                '"' => break,
                c => item.push(c),
            }
        }
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek() == Some(&':') {
            keys.push(item);
        }
    }
    keys
}

/// Whether `text` matches the glob `pattern`, where `*` stands for any run
/// of characters, none included.
fn glob(pattern: &str, text: &str) -> bool {
    let Some((head, tail)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(rest) = text.strip_prefix(head) else {
        return false;
    };
    (0..=rest.len())
        .filter(|&i| rest.is_char_boundary(i))
        .any(|i| glob(tail, &rest[i..]))
}

/// Whether the permission rule `rule` allows running `command`, as Claude
/// Code matches a `Bash` rule: `Bash` alone allows everything, `*` stands
/// for anything, a closing ` *` also allows the bare command, and the older
/// closing `:*` allows anything that starts with what comes before it. A
/// deny rule matches a command the same way.
fn allows(rule: &str, command: &str) -> bool {
    let pattern = match rule {
        "Bash" => "*",
        _ => match rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) {
            Some(p) => p,
            None => return false,
        },
    };
    if let Some(prefix) = pattern.strip_suffix(":*") {
        return command.starts_with(prefix);
    }
    pattern.strip_suffix(" *") == Some(command) || glob(pattern, command)
}

/// The value of the shell assignment `name=value` in `script`, unquoted.
fn assignment(script: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    script.lines().find_map(|l| {
        let value = l.trim().strip_prefix(&prefix)?;
        Some(value.trim_matches(['"', '\'']).to_owned())
    })
}

/// The agent instructions, as (path, text): `AGENTS.md`, the index and
/// every rule file.
fn agent_texts() -> Vec<(String, String)> {
    let root = repo_root();
    let mut paths = vec![AGENTS.to_owned(), INDEX.to_owned()];
    paths.extend(rule_files().iter().map(|r| format!("{RULES}/{r}.md")));
    paths
        .into_iter()
        .map(|p| {
            let text = read(&root.join(&p));
            (p, text)
        })
        .collect()
}

#[test]
fn the_rules_folder_holds_the_always_loaded_rules() {
    let rules = rule_files();
    for name in ALWAYS_LOADED {
        assert!(
            rules.iter().any(|r| r == name),
            "{RULES}/{name}.md must exist (found: {rules:?})"
        );
    }
}

#[test]
fn the_index_names_every_rule_file() {
    let named = named_rules(&read(&repo_root().join(INDEX)));
    let missing: Vec<String> = rule_files()
        .into_iter()
        .filter(|r| !named.contains(r))
        .collect();
    assert!(
        missing.is_empty(),
        "{INDEX} must name every rule file as `rules/<name>.md`; missing: {missing:?}"
    );
}

#[test]
fn every_rule_file_the_index_names_exists() {
    let rules = rule_files();
    let gone: Vec<String> = named_rules(&read(&repo_root().join(INDEX)))
        .into_iter()
        .filter(|r| !rules.contains(r))
        .collect();
    assert!(
        gone.is_empty(),
        "{INDEX} names rule files that don't exist in {RULES}: {gone:?}"
    );
}

#[test]
fn the_index_imports_the_privacy_and_clean_room_rules() {
    let text = read(&repo_root().join(INDEX));
    for name in ALWAYS_LOADED {
        assert!(
            imports(&text, &format!("rules/{name}.md")),
            "{INDEX} must import `@rules/{name}.md` on a line of its own, outside code blocks"
        );
    }
}

#[test]
fn the_root_claude_md_imports_the_index() {
    let text = read(&repo_root().join(ROOT_CLAUDE));
    assert!(
        imports(&text, INDEX),
        "{ROOT_CLAUDE} must import `@{INDEX}` on a line of its own"
    );
}

#[test]
fn agents_md_points_to_the_index_and_names_every_rule_file() {
    let text = read(&repo_root().join(AGENTS));
    assert!(text.contains(INDEX), "{AGENTS} must point to {INDEX}");
    let named = named_rules(&text);
    let missing: Vec<String> = rule_files()
        .into_iter()
        .filter(|r| !named.contains(r))
        .collect();
    assert!(
        missing.is_empty(),
        "{AGENTS} must list every rule file; missing: {missing:?}"
    );
}

#[test]
fn the_shared_settings_allow_only_safe_commands() {
    let json = read(&repo_root().join(SETTINGS));
    let allow = string_array(&json, "allow").unwrap_or_default();
    let unsafe_rules: Vec<&String> = allow
        .iter()
        .filter(|r| !SAFE_ALLOW_RULES.contains(&r.as_str()))
        .collect();
    assert!(
        unsafe_rules.is_empty(),
        "{SETTINGS} is shared and a checkout may be a stranger's pull request, so it allows \
         only {SAFE_ALLOW_RULES:?}. Allow these in your own .claude/settings.local.json \
         instead: {unsafe_rules:?}"
    );
}

#[test]
fn the_shared_settings_allow_no_command_that_writes_or_runs_code() {
    let json = read(&repo_root().join(SETTINGS));
    let allow = string_array(&json, "allow").unwrap_or_default();
    let mut hits = Vec::new();
    for rule in &allow {
        for command in NOT_ALLOWED {
            if allows(rule, command) {
                hits.push(format!("{rule} allows `{command}`"));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "{SETTINGS} is shared: allow these in .claude/settings.local.json instead:\n  {}",
        hits.join("\n  ")
    );
}

#[test]
fn no_safe_rule_allows_a_command_that_writes_or_runs_code() {
    for rule in SAFE_ALLOW_RULES {
        for command in NOT_ALLOWED {
            assert!(!allows(rule, command), "{rule} allows `{command}`");
        }
    }
}

#[test]
fn the_shared_settings_deny_git_output() {
    let json = read(&repo_root().join(SETTINGS));
    let deny = string_array(&json, "deny").unwrap_or_default();
    let mut open = Vec::new();
    for command in WRITES_THROUGH_OUTPUT {
        if !deny.iter().any(|rule| allows(rule, command)) {
            open.push(command);
        }
    }
    assert!(
        open.is_empty(),
        "{SETTINGS} allows git's read commands, so it must deny `{OUTPUT_DENY}`: \
         these write a file: {open:?}"
    );
}

#[test]
fn the_output_deny_rule_blocks_every_form_and_no_plain_read() {
    for command in WRITES_THROUGH_OUTPUT {
        assert!(
            SAFE_ALLOW_RULES.iter().any(|rule| allows(rule, command)),
            "a safe rule should allow `{command}`, or the deny rule isn't needed for it"
        );
        assert!(
            allows(OUTPUT_DENY, command),
            "{OUTPUT_DENY} misses `{command}`"
        );
    }
    for command in [
        "git diff",
        "git diff HEAD~1 -- src",
        "git log -1 --format=%h",
        "git show HEAD:README.md",
        "git status --short",
    ] {
        assert!(
            !allows(OUTPUT_DENY, command),
            "{OUTPUT_DENY} blocks `{command}`"
        );
    }
}

#[test]
fn the_output_deny_rule_misses_quoted_forms_and_the_rules_say_so() {
    for command in DODGES_OUTPUT_DENY {
        assert!(
            SAFE_ALLOW_RULES.iter().any(|rule| allows(rule, command)),
            "a safe rule should allow `{command}`"
        );
        assert!(
            !allows(OUTPUT_DENY, command),
            "{OUTPUT_DENY} now blocks `{command}`; update {CONVENTIONS} and this test"
        );
    }
    assert!(
        read(&repo_root().join(CONVENTIONS)).contains(QUOTING_LIMIT),
        "{CONVENTIONS} must say that {QUOTING_LIMIT} gets past {OUTPUT_DENY}"
    );
}

#[test]
fn the_shared_settings_hold_only_permission_lists() {
    let json = read(&repo_root().join(SETTINGS));
    let other: Vec<String> = json_keys(&json)
        .into_iter()
        .filter(|k| !SETTINGS_KEYS.contains(&k.as_str()))
        .collect();
    assert!(
        other.is_empty(),
        "{SETTINGS} may hold only {SETTINGS_KEYS:?}, so it can't run code or skip a \
         question; move these to .claude/settings.local.json: {other:?}"
    );
}

#[test]
fn the_shared_settings_keep_the_research_deny_rule() {
    let json = read(&repo_root().join(SETTINGS));
    let deny = string_array(&json, "deny").unwrap_or_default();
    assert!(
        deny.iter().any(|r| r == RESEARCH_DENY),
        "{SETTINGS} must deny `{RESEARCH_DENY}` (clean-room rule); deny is {deny:?}"
    );
}

#[test]
fn the_rules_describe_the_public_repository() {
    assert!(
        read(&repo_root().join(BRANCHES)).contains(&format!("`{REPOSITORY}`")),
        "{BRANCHES} must say the project lives at `{REPOSITORY}`"
    );
    let mut found = Vec::new();
    for (path, text) in agent_texts() {
        let lower = text.to_lowercase();
        for phrase in PRE_PUBLIC {
            if lower.contains(phrase) {
                found.push(format!("{path}: {phrase:?}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "the repository is public; reword:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_licensing_rule_installs_the_cargo_about_the_notices_script_needs() {
    let script = read(&repo_root().join(NOTICES_SCRIPT));
    let Some(version) = assignment(&script, "ABOUT_VERSION") else {
        panic!("{NOTICES_SCRIPT} must set ABOUT_VERSION");
    };
    let install = format!("cargo install cargo-about --locked --features cli --version {version}");
    assert!(
        read(&repo_root().join(LICENSING)).contains(&install),
        "{LICENSING} must give the install command {NOTICES_SCRIPT} needs: `{install}`"
    );
}

#[test]
fn allows_matches_rules_as_claude_code_does() {
    assert!(allows("Bash(git add *)", "git add ."));
    assert!(allows("Bash(git add *)", "git add"));
    assert!(allows("Bash(git add:*)", "git add ."));
    assert!(allows("Bash(git branch *)", "git branch -D dev"));
    assert!(allows("Bash(git checkout -b *)", "git checkout -b topic"));
    assert!(allows("Bash(git *)", "git commit -m x"));
    assert!(allows("Bash(cargo fmt *)", "cargo fmt"));
    assert!(allows("Bash(cargo * --check)", "cargo fmt --all --check"));
    assert!(allows("Bash(*)", "cargo run"));
    assert!(allows("Bash", "cargo run"));
    assert!(allows("Bash(rustc --version)", "rustc --version"));
    assert!(!allows("Bash(cargo fmt --check *)", "cargo fmt"));
    assert!(!allows("Bash(cargo fmt --check *)", "cargo fmt --all"));
    assert!(!allows(
        "Bash(git branch --show-current)",
        "git branch -D dev"
    ));
    assert!(!allows("Bash(git status *)", "git statusx"));
    assert!(!allows("Bash(git status *)", "git add ."));
    assert!(!allows("Bash(cargo bench *)", "cargo run"));
    assert!(!allows("Read(**/x.md)", "git add ."));
}

#[test]
fn json_keys_reads_object_keys_only() {
    let json = r#"{ "permissions": { "allow": ["Bash(a:b)", "x"], "deny" : [] }, "hooks": {} }"#;
    assert_eq!(json_keys(json), ["permissions", "allow", "deny", "hooks"]);
}

#[test]
fn assignment_reads_a_shell_value() {
    let script = "# ABOUT_VERSION=0.1\nABOUT_VERSION=0.9.3\nX=\"y\"\n";
    assert_eq!(
        assignment(script, "ABOUT_VERSION").as_deref(),
        Some("0.9.3")
    );
    assert_eq!(assignment(script, "X").as_deref(), Some("y"));
    assert_eq!(assignment(script, "Z"), None);
}

#[test]
fn named_rules_reads_rule_paths() {
    let text =
        "@rules/privacy.md, `.agents/rules/runs.md`, the `.agents/rules/` folder, rules/x.txt";
    assert_eq!(named_rules(text), ["privacy", "runs"]);
}
