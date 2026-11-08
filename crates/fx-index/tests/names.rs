//! Contract tests for `NameBuffer`: one contiguous buffer of NUL-separated
//! UTF-8 names plus a `u32` offset per name (docs/PLAN.md, data-layout table:
//! "Name buffer (NUL-separated)" and "Name offset").

use fx_index::NameBuffer;

/// Pushes every name and returns the indices `push` handed out.
fn fill(buf: &mut NameBuffer, names: &[&str]) -> Vec<u32> {
    names
        .iter()
        .map(|n| buf.push(n).expect("valid name must be accepted"))
        .collect()
}

#[test]
fn new_buffer_is_empty() {
    let buf = NameBuffer::new();
    assert_eq!(buf.len(), 0);
    assert!(buf.bytes().is_empty());
}

#[test]
fn push_returns_dense_indices_and_get_round_trips() {
    let names = ["readme.md", "Cargo.toml", "a", "photo 2024.JPG", ".bashrc"];
    let mut buf = NameBuffer::new();
    let ids = fill(&mut buf, &names);

    assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    assert_eq!(buf.len(), names.len());
    for (id, name) in ids.iter().zip(names.iter()) {
        assert_eq!(buf.get(*id), *name);
    }
}

#[test]
fn non_ascii_names_round_trip_byte_exact_without_normalization() {
    let nfc = "caf\u{e9}.txt"; // é as one code point
    let nfd = "cafe\u{301}.txt"; // e + combining acute accent
    assert_ne!(nfc.as_bytes(), nfd.as_bytes());

    let names = [
        nfc,
        nfd,
        "日本語のファイル.pdf",
        "Ünïcödé — dash",
        "emoji 🦀📁.rs",
        "Ελληνικά",
    ];
    let mut buf = NameBuffer::new();
    let ids = fill(&mut buf, &names);

    for (id, name) in ids.iter().zip(names.iter()) {
        let got = buf.get(*id);
        assert_eq!(got.as_bytes(), name.as_bytes(), "name {id} changed");
    }
    // NFC and NFD stay distinct entries; the buffer must not normalize.
    assert_ne!(buf.get(ids[0]), buf.get(ids[1]));
}

#[test]
fn rejects_names_containing_nul() {
    let mut buf = NameBuffer::new();
    buf.push("before").unwrap();
    let bytes_before = buf.bytes().to_vec();

    assert!(buf.push("bad\0name").is_err());
    assert!(buf.push("\0").is_err());
    assert!(buf.push("trailing\0").is_err());
    assert!(buf.push("\0leading").is_err());

    // A rejected push leaves the buffer untouched.
    assert_eq!(buf.len(), 1);
    assert_eq!(buf.bytes(), bytes_before.as_slice());
    assert_eq!(buf.get(0), "before");

    // And the next valid push still gets the next dense index.
    assert_eq!(buf.push("after").unwrap(), 1);
    assert_eq!(buf.get(1), "after");
}

#[test]
fn rejects_empty_names() {
    let mut buf = NameBuffer::new();
    assert!(buf.push("").is_err());
    assert_eq!(buf.len(), 0);
    assert!(buf.bytes().is_empty());

    buf.push("x").unwrap();
    assert!(buf.push("").is_err());
    assert_eq!(buf.len(), 1);
    assert_eq!(buf.bytes(), b"x");
}

#[test]
fn buffer_layout_is_names_joined_by_nul() {
    let names = ["alpha", "b", "ga\u{e9}mma.txt", "delta.tar.gz"];
    let mut buf = NameBuffer::new();
    fill(&mut buf, &names);

    let expected = names.join("\0");
    assert_eq!(buf.bytes(), expected.as_bytes());

    // Exactly one NUL between consecutive names, and no NUL inside a name.
    let nul_count = buf.bytes().iter().filter(|&&b| b == 0).count();
    assert_eq!(nul_count, names.len() - 1);
}

#[test]
fn offsets_point_at_each_name_and_ascend() {
    let names = ["one", "two", "three", "f\u{f6}ur", "5"];
    let mut buf = NameBuffer::new();
    let ids = fill(&mut buf, &names);
    let offsets = buf.offsets();
    let bytes = buf.bytes();

    // One offset per name (an optional end sentinel is allowed).
    assert!(
        offsets.len() == names.len() || offsets.len() == names.len() + 1,
        "expected {} or {} offsets, got {}",
        names.len(),
        names.len() + 1,
        offsets.len()
    );
    assert_eq!(offsets[0], 0, "first name starts at byte 0");
    for w in offsets.windows(2) {
        assert!(w[0] < w[1], "offsets must strictly ascend: {offsets:?}");
    }

    // Each offset is the start of its name, and the name runs to the next NUL
    // (or the end of the buffer) — the zero-copy scan fx-query does.
    for (id, name) in ids.iter().zip(names.iter()) {
        let start = offsets[*id as usize] as usize;
        let end = bytes[start..]
            .iter()
            .position(|&b| b == 0)
            .map_or(bytes.len(), |p| start + p);
        assert_eq!(&bytes[start..end], name.as_bytes());
        if start > 0 {
            assert_eq!(bytes[start - 1], 0, "name {id} not preceded by NUL");
        }
    }
}

#[test]
fn many_names_round_trip_with_ascending_offsets() {
    let names: Vec<String> = (0..20_000u32)
        .map(|i| match i % 4 {
            0 => format!("file_{i}.txt"),
            1 => format!("Caf\u{e9} {i}.JPG"),
            2 => format!("cafe\u{301}-{i}"),
            _ => format!(".hidden{i}"),
        })
        .collect();
    let mut buf = NameBuffer::new();
    for (i, n) in names.iter().enumerate() {
        assert_eq!(buf.push(n).unwrap() as usize, i);
    }

    assert_eq!(buf.len(), names.len());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(buf.get(i as u32), n.as_str());
    }
    for w in buf.offsets().windows(2) {
        assert!(w[0] < w[1]);
    }
    let joined = names.join("\0");
    assert_eq!(buf.bytes(), joined.as_bytes());
}

#[test]
fn heap_bytes_covers_names_and_offsets() {
    let mut buf = NameBuffer::new();
    let mut name_bytes = 0usize;
    for i in 0..10_000u32 {
        let n = format!("name-{i}.dat");
        name_bytes += n.len();
        buf.push(&n).unwrap();
    }
    let n = buf.len();
    // At least the names, their separators and a 4-byte offset each.
    let minimum = name_bytes + (n - 1) + 4 * n;
    assert!(
        buf.heap_bytes() >= minimum,
        "heap_bytes {} < minimum {}",
        buf.heap_bytes(),
        minimum
    );
    // And not wildly more than that (Vec growth at most doubles capacity).
    assert!(
        buf.heap_bytes() <= 2 * minimum + 4096,
        "heap_bytes {} is far above {}",
        buf.heap_bytes(),
        minimum
    );
}
