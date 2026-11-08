//! The third-party notices: `THIRD-PARTY-NOTICES.md` at the repository's
//! root, written by `scripts/third-party-notices.sh` from `Cargo.lock`. The
//! app carries the text in its binary, shows it in
//! the About panel's credits ([`about_credits`]) and writes it into its
//! bundle's `Contents/Resources` ([`crate::bundle`]), so every copy of the
//! app keeps the notices its open-source crates ask for.

/// The notices, as the repository holds them (Markdown).
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

/// `notices` as plain text for the About panel, which shows text, not
/// Markdown: code fences go, and outside them headings lose their `#`
/// marks and inline code its backticks. The licence texts inside the
/// fences stay exactly as they are.
pub fn about_credits(notices: &str) -> String {
    let mut out = String::with_capacity(notices.len());
    let mut in_fence = false;
    for line in notices.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push_str(line);
        } else if line.starts_with('#') {
            out.push_str(&line.trim_start_matches('#').trim_start().replace('`', ""));
        } else {
            out.push_str(&line.replace('`', ""));
        }
        out.push('\n');
    }
    out
}
