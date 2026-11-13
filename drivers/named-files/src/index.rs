//! The names this driver serves, their bounds, and the index format (`docs/daemon.md`).
//!
//! The index is Orion's text format, used as Orion writes it:
//!
//! ```text
//! orion-index 1
//! total <bytes>
//! <name> <max_bytes> user-data|no-user-data
//! ```
//!
//! `total` is the sum of the listed names' bounds. Lines follow the table's order; no reader relies
//! on it.

use std::fmt::Write;

/// A name this driver can serve: where its bytes come from is decided by [`crate::producers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Name {
    Boots,
    Storage,
    TrialReason,
    TreeOverlay,
    TreeBoot,
    TreeModprobe,
    Kernel,
}

/// Every name, in the order the index lists them.
pub const NAMES: [Name; 7] = [
    Name::Boots,
    Name::Storage,
    Name::TrialReason,
    Name::TreeOverlay,
    Name::TreeBoot,
    Name::TreeModprobe,
    Name::Kernel,
];

/// The index's own name.
pub const INDEX: &str = "lemnos/index";

impl Name {
    /// The name as a reader asks for it.
    pub fn text(self) -> &'static str {
        match self {
            Name::Boots => "lemnos/boots",
            Name::Storage => "lemnos/storage",
            Name::TrialReason => "lemnos/trial-reason",
            Name::TreeOverlay => "lemnos/tree/overlay.dts",
            Name::TreeBoot => "lemnos/tree/boot.txt",
            Name::TreeModprobe => "lemnos/tree/modprobe.conf",
            Name::Kernel => "lemnos/kernel",
        }
    }

    /// The most bytes this name's file may take. A file over its bound is refused.
    pub fn bound(self) -> usize {
        match self {
            Name::Boots => 16 << 10,
            Name::Storage => 4 << 10,
            Name::TrialReason => 4 << 10,
            Name::TreeOverlay => 256 << 10,
            Name::TreeBoot => 64 << 10,
            Name::TreeModprobe => 16 << 10,
            Name::Kernel => 16 << 10,
        }
    }

    /// Whether the file may hold user data: anything a person should be told about before a
    /// bundle is sent. None of these names do.
    pub fn user_data(self) -> bool {
        false
    }

    /// The name with this text, if the driver serves it.
    pub fn named(text: &str) -> Option<Name> {
        NAMES.into_iter().find(|name| name.text() == text)
    }
}

/// The index text for `served`, the names a node serves now.
pub fn render(served: &[Name]) -> String {
    let total: usize = served.iter().map(|name| name.bound()).sum();
    let mut out = format!("orion-index 1\ntotal {total}\n");
    for name in served {
        let flag = if name.user_data() {
            "user-data"
        } else {
            "no-user-data"
        };
        let _ = writeln!(out, "{} {} {flag}", name.text(), name.bound());
    }
    out
}

/// One line of an index, as a reader parses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub name: String,
    pub max_bytes: u64,
    pub user_data: bool,
}

/// Parses an index: its `total` and its lines. `None` when it is not an `orion-index 1` text.
pub fn parse(text: &str) -> Option<(u64, Vec<Line>)> {
    let mut lines = text.lines();
    if lines.next()? != "orion-index 1" {
        return None;
    }
    let total = lines.next()?.strip_prefix("total ")?.parse().ok()?;
    let mut out = Vec::new();
    for line in lines {
        let mut parts = line.split(' ');
        let name = parts.next()?.to_string();
        let max_bytes = parts.next()?.parse().ok()?;
        let user_data = match parts.next()? {
            "user-data" => true,
            "no-user-data" => false,
            _ => return None,
        };
        out.push(Line {
            name,
            max_bytes,
            user_data,
        });
    }
    Some((total, out))
}
