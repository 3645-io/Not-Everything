//! `highlight_bench`: the cost of match highlighting
//! (`fx_query::highlight`), per query and per result row.
//!
//! ```text
//! cargo run --release -p fx-query --example highlight_bench
//! cargo run --release -p fx-query --example highlight_bench -- --names 10000 --passes 20
//! ```
//!
//! Setup, outside any timing: `fx_bench::generate_tree` builds `--names`
//! entries; each row is an entry's folder path and name. A second set
//! gives every name an accent and a curly apostrophe (`Cé…’s`), so the
//! non-ASCII mapping is measured too.
//!
//! Per query it reports the time to prepare the highlighter once
//! (`Highlighter::for_query`), and the time per row of
//! `Highlighter::highlight`: the mean over `--passes` passes over all rows
//! and the slowest pass's mean, plus how many rows got a highlight. The
//! window highlights only the rows it shows, a few dozen per frame.
//!
//! Exit codes: 0 after a completed run, 2 for bad arguments.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fx_bench::{MachineInfo, TreeShape, generate_tree};
use fx_query::{Highlighter, QueryOptions};

const USAGE: &str = "\
usage:
  highlight_bench [--names N] [--passes P] [--seed S]

  --names N    rows (synthetic entries), 1..=10000000 (default 10000)
  --passes P   timed passes over all rows, at least 1 (default 20)
  --seed S     generator seed (default 1)";

const PLAIN: QueryOptions = QueryOptions {
    match_case: false,
    match_path: false,
    regex: false,
};

/// Label, query and options.
const QUERIES: &[(&str, &str, QueryOptions)] = &[
    ("literal", "conf", PLAIN),
    ("1-char", "e", PLAIN),
    ("two terms", "data json", PLAIN),
    ("accent", "café's", PLAIN),
    ("path term", "path:resources/", PLAIN),
    (
        "path chip",
        "lib",
        QueryOptions {
            match_path: true,
            ..PLAIN
        },
    ),
    ("wildcard", "*.png", PLAIN),
    ("regex term", r"regex:\d+", PLAIN),
    (
        "regex chip",
        r"img_\d+\.jpg$",
        QueryOptions {
            regex: true,
            ..PLAIN
        },
    ),
];

struct Args {
    names: usize,
    passes: u32,
    seed: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        names: 10_000,
        passes: 20,
        seed: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--names" => args.names = value()?.parse().map_err(|e| format!("--names: {e}"))?,
            "--passes" => args.passes = value()?.parse().map_err(|e| format!("--passes: {e}"))?,
            "--seed" => args.seed = value()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if args.names == 0 || args.names > 10_000_000 || args.passes == 0 {
        return Err("--names must be 1..=10000000 and --passes at least 1".into());
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("highlight_bench: {msg}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let tree = generate_tree(args.names, args.seed, TreeShape::default());
    let mut ascii = Vec::with_capacity(tree.len());
    for i in 0..tree.len() {
        let path = format!("/{}", tree.path(i));
        let name = tree.names.get(i).to_string();
        let folder = path[..path.len() - name.len()]
            .trim_end_matches('/')
            .to_string();
        ascii.push((folder, name, tree.is_dir[i]));
    }
    let accented: Vec<(String, String, bool)> = ascii
        .iter()
        .map(|(folder, name, dir)| {
            let mut chars = name.chars();
            let first = chars.next().map(String::from).unwrap_or_default();
            (
                folder.clone(),
                format!("{first}é{}’s", chars.as_str()),
                *dir,
            )
        })
        .collect();

    let m = MachineInfo::detect();
    println!(
        "highlight_bench: {} rows, {} passes, seed {}; {} {}, {}; {} build",
        ascii.len(),
        args.passes,
        args.seed,
        m.os,
        m.arch,
        m.cpu_brand,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!(
        "{:<12} {:<18} {:>10} {:>8} {:>12} {:>12} {:>8}",
        "query", "text", "set", "prep_us", "ns_per_row", "worst_pass", "hits"
    );
    for (set, rows) in [("ascii", &ascii), ("accented", &accented)] {
        for &(label, query, options) in QUERIES {
            let start = Instant::now();
            let h = Highlighter::for_query(query, options);
            let prepare = start.elapsed();
            let mut hits = 0;
            for (folder, name, dir) in rows.iter() {
                hits += usize::from(!h.highlight(folder, name, *dir).is_empty());
            }
            let mut total = Duration::ZERO;
            let mut worst = Duration::ZERO;
            for _ in 0..args.passes {
                let t = Instant::now();
                for (folder, name, dir) in rows.iter() {
                    black_box(h.highlight(black_box(folder), black_box(name), *dir));
                }
                let pass = t.elapsed();
                total += pass;
                worst = worst.max(pass);
            }
            let per_row = |d: Duration| d.as_nanos() as f64 / rows.len() as f64;
            println!(
                "{:<12} {:<18} {:>10} {:>8.1} {:>12.1} {:>12.1} {:>8}",
                label,
                format!("{query:?}"),
                set,
                prepare.as_nanos() as f64 / 1000.0,
                per_row(total) / f64::from(args.passes),
                per_row(worst),
                hits
            );
        }
    }
    ExitCode::SUCCESS
}
