//! The boot partition's run-time write audit, as a test: every non-test line of the run-time sources
//! that writes under a boot-partition path must be one of the audited lines below, each with its
//! verdict. A new line fails the test and names itself, so a run-time write to the boot partition
//! cannot come back unreviewed. The audit table is `docs/slots.md` ("Boot partition writes audit").

use std::fs;
use std::path::{Path, PathBuf};

/// The run-time sources: the crates the daemon and slot-init run.
const SOURCE_DIRS: [&str; 4] = [
    "drivers/slot-store/src",
    "drivers/system-slots/src",
    "service/slot-init/src",
    "service/lemnosd/src",
];

/// Tokens that make a line a write.
const WRITES: [&str; 8] = [
    "replace(",
    "rename(",
    "fs::write(",
    "fs::copy(",
    "remove_file(",
    "remove_dir_all(",
    "create_dir_all(",
    "sync_dir(",
];

/// Tokens that make a line a path on the boot partition.
/// (The dotted and closed forms, so `layout.bootfiles()` and `layout.boot_list()`, which are on the
/// data partition, are not taken for it.)
const BOOT_PATHS: [&str; 6] = [
    "layout.boot.",
    "layout.boot)",
    "layout.boot_dir(",
    "self.boot.",
    "self.boot)",
    "boot.join(",
];

/// The audited lines (trimmed), each `file: line` with its verdict. Verdicts: `install` is an
/// install-time write that needs a ruling (see docs/slots.md); `read` is a read path (not a write, a
/// file-level read the audit keeps on the list); `nand` is the UBI form's root copy.
const AUDITED: [(&str, &str); 2] = [
    ("drivers/system-slots/src/uboot.rs", "sync_dir(&self.boot)"),
    (
        "drivers/system-slots/src/uboot.rs",
        "replace(&dir.join(\"extlinux.conf\"), &text)?;",
    ),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The non-test source files under `dir`: every `.rs` file, but not the test modules.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "tests") {
                sources(&path, out);
            }
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_some_and(|e| e == "rs")
            && !name.ends_with("tests.rs")
            && !name.ends_with("_tests.rs")
            && name != "tests.rs"
            && name != "boot_writes_audit.rs"
        {
            out.push(path);
        }
    }
}

/// The run-time write lines on a boot path that are not in the audit (`file: line`).
fn unaudited() -> Vec<String> {
    let mut files = Vec::new();
    for dir in SOURCE_DIRS {
        sources(&root().join(dir), &mut files);
    }
    let mut out = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = fs::read_to_string(&path).unwrap();
        // Everything after the test module starts is not run-time code.
        let body = text.split("#[cfg(test)]").next().unwrap_or("");
        for line in body.lines() {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            let writes = WRITES.iter().any(|w| line.contains(w));
            let boot = BOOT_PATHS.iter().any(|p| line.contains(p));
            if writes && boot && !AUDITED.iter().any(|(f, l)| *f == rel && *l == line) {
                out.push(format!("{rel}: {line}"));
            }
        }
    }
    out
}

/// Every run-time write under a boot path is audited, and every audited line still exists.
#[test]
fn every_run_time_write_to_the_boot_partition_is_audited() {
    let found = unaudited();
    assert!(
        found.is_empty(),
        "unaudited run-time write(s) to a boot path (audit them in docs/slots.md, then add the line to AUDITED):\n{}",
        found.join("\n")
    );
}

/// The audit's lines are current: an audited line that no longer exists is a stale entry.
#[test]
fn the_audit_has_no_stale_lines() {
    let mut texts = String::new();
    let mut files = Vec::new();
    for dir in SOURCE_DIRS {
        sources(&root().join(dir), &mut files);
    }
    for path in files {
        texts.push_str(&fs::read_to_string(path).unwrap());
    }
    for (file, line) in AUDITED {
        assert!(texts.contains(line), "stale audit line in {file}: {line}");
    }
}

/// The FAT mount of the boot partition at run time is read-only everywhere in the run-time sources.
#[test]
fn no_run_time_source_mounts_the_boot_partition_read_write() {
    let mut files = Vec::new();
    for dir in SOURCE_DIRS {
        sources(&root().join(dir), &mut files);
    }
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        let body = text.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            !body.contains("mount_boot(true)"),
            "{}: a read-write mount of the boot partition",
            path.display()
        );
    }
}
