//! Contract tests for `fx_ui_macos::notices`: the app carries
//! `THIRD-PARTY-NOTICES.md` and shows it as plain text in the About panel's
//! credits.

use fx_ui_macos::notices::{THIRD_PARTY_NOTICES, about_credits};

#[test]
fn the_app_carries_the_repositorys_notices() {
    let file = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../THIRD-PARTY-NOTICES.md"),
    )
    .expect("THIRD-PARTY-NOTICES.md exists");
    assert_eq!(THIRD_PARTY_NOTICES, file);
}

/// The About panel shows text, not Markdown: headings lose their `#`
/// marks, code fences go, inline code loses its backticks, and the rest
/// stays as it is.
#[test]
fn the_credits_are_the_notices_as_plain_text() {
    let markdown = "# Third-party notices\n\nIntro line.\n\n## MIT License\n\nUsed by:\n\n- `regex 1.13.1`\n\n```text\nCopyright (c) Someone\n\n    indented\n```\n";
    assert_eq!(
        about_credits(markdown),
        "Third-party notices\n\nIntro line.\n\nMIT License\n\nUsed by:\n\n- regex 1.13.1\n\nCopyright (c) Someone\n\n    indented\n"
    );
}

#[test]
fn the_real_credits_hold_no_markdown_marks() {
    let credits = about_credits(THIRD_PARTY_NOTICES);
    assert!(credits.starts_with("Third-party notices\n"));
    assert!(
        !credits
            .lines()
            .any(|l| l.starts_with("```") || l.starts_with('#'))
    );
}
