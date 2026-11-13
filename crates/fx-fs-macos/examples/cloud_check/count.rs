//! Stub: the signatures only, so the tests compile and fail first.

use std::collections::BTreeSet;
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Census {
    pub dataless: BTreeSet<PathBuf>,
    pub unreadable: u64,
}

pub fn census(_root: &Path, _dataless: &dyn Fn(&Path, &Metadata) -> bool) -> io::Result<Census> {
    todo!()
}

pub fn downloads(_before: &Census, _dataless_now: &dyn Fn(&Path) -> Option<bool>) -> u64 {
    todo!()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub dataless_before: u64,
    pub dataless_after: u64,
    pub skipped_dataless: u64,
    pub edeadlk: u64,
    pub downloads: u64,
}

impl Report {
    pub fn lines(&self) -> String {
        todo!()
    }

    pub fn exit_code(&self) -> u8 {
        todo!()
    }
}
