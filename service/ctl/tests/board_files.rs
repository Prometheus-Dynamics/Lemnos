//! Every board file in the repository validates, and the daemon's `--check` agrees with `lemnos-ctl
//! validate` on it. The table below names each board file and its expected outcome, exactly. A new
//! board file that is not in the table fails the test; a listed file whose outcome changes (a fix
//! that makes a refused file pass, or a regression) fails it too, so the table cannot rot.
//!
//! A board file is one whose text declares `format = "lemnos.board"`, the header the board parser
//! reads. The walk covers the whole repository except build output and tool state.

use std::path::{Path, PathBuf};

/// What `lemnos-ctl validate --no-live` says about a board file.
#[derive(Debug, Clone, Copy)]
enum Validate {
    /// Accepted, with no refusal.
    Pass,
    /// Refused with exactly this reason (the text after `<path>: `).
    Refused(&'static str),
}

/// What the daemon's `--check` says about a board file.
#[derive(Debug, Clone, Copy)]
enum Daemon {
    /// Accepted, with no refusal.
    Pass,
    /// Refused with exactly this reason.
    Refused(&'static str),
    /// Accepted, or refused with a reason starting with this text. Used only where the outcome
    /// depends on the machine the check runs on, and the file's validation does not.
    Machine(&'static str),
}

struct Row {
    file: &'static str,
    validate: Validate,
    daemon: Daemon,
}

/// The gadget of an rk board file has no `interfaces` text. Next takes no default composition, so
/// the daemon refuses the gadget alone (the board runs without it). The text is a product choice,
/// not a fact the repository holds: the board file is left as it is until one is given.
const RK_GADGET_NO_INTERFACES: &str =
    "device \"gadget\": setting \"interfaces\" is required: give the interfaces text";

const TABLE: &[Row] = &[
    Row {
        file: "docs/examples/full-board.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "docs/examples/package-board.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "docs/examples/sim-board.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "service/devtree/tests/fixtures/all-kinds-board.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "testing/bench/board.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "testing/boards/guest.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "testing/boards/k230.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    Row {
        file: "testing/boards/pisp-camera-chain.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
    // The daemon reads the board's serial from the machine's device tree when the gadget derives
    // its addresses from it. A board has that file; a development machine may not, and then the
    // check says so.
    Row {
        file: "testing/boards/reference.toml",
        validate: Validate::Pass,
        daemon: Daemon::Machine(
            "device \"gadget\": Status(-22): the board serial is not known: cannot read /proc/device-tree/serial-number",
        ),
    },
    Row {
        file: "testing/boards/rk3566.toml",
        validate: Validate::Refused(RK_GADGET_NO_INTERFACES),
        daemon: Daemon::Refused(
            "device \"gadget\": setting \"interfaces\" is required: give the interfaces text; needs a restart of the daemon to retry",
        ),
    },
    Row {
        file: "testing/boards/rk3576.toml",
        validate: Validate::Refused(RK_GADGET_NO_INTERFACES),
        daemon: Daemon::Refused(
            "device \"gadget\": setting \"interfaces\" is required: give the interfaces text; needs a restart of the daemon to retry",
        ),
    },
    Row {
        file: "testing/boards/rk3588.toml",
        validate: Validate::Refused(RK_GADGET_NO_INTERFACES),
        daemon: Daemon::Refused(
            "device \"gadget\": setting \"interfaces\" is required: give the interfaces text; needs a restart of the daemon to retry",
        ),
    },
    Row {
        file: "testing/boards/sun60i-a733.toml",
        validate: Validate::Pass,
        daemon: Daemon::Pass,
    },
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root")
}

/// Every board file under `dir`, as a path relative to the repository root, sorted.
fn board_files(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && name != ".git" && !name.starts_with('.') {
                board_files(&path, root, found);
            }
        } else if path.extension().is_some_and(|e| e == "toml") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text
                .lines()
                .any(|line| line.trim_start().starts_with("format = \"lemnos.board\""))
            {
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                found.push(relative);
            }
        }
    }
}

fn all_board_files() -> Vec<String> {
    let root = repo();
    let mut found = Vec::new();
    board_files(&root, &root, &mut found);
    found.sort();
    found
}

/// `lemnos-ctl validate --no-live` on the file: `Ok`, or the refusal's reason (the text after the
/// path).
fn validate(path: &Path) -> Result<(), String> {
    let mut out = Vec::new();
    let args = [
        "validate".to_string(),
        "--no-live".to_string(),
        path.display().to_string(),
    ];
    lemnos_ctl::run(&args, &mut out).map_err(|error| {
        let prefix = format!("{}: ", path.display());
        error.strip_prefix(&prefix).unwrap_or(&error).to_string()
    })
}

/// The daemon's `--check` on the file's text, with the built-in drivers.
fn daemon(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).expect("a board file");
    lemnosd::check(&text, &lemnosd::builtin::drivers())
}

fn row_for(file: &str) -> Option<&'static Row> {
    TABLE.iter().find(|row| row.file == file)
}

#[test]
fn the_table_names_every_board_file_and_only_board_files() {
    let found = all_board_files();
    let listed: Vec<String> = TABLE.iter().map(|row| row.file.to_string()).collect();
    let missing: Vec<&String> = found.iter().filter(|f| !listed.contains(f)).collect();
    let stale: Vec<&String> = listed.iter().filter(|f| !found.contains(f)).collect();
    assert!(
        missing.is_empty(),
        "board files not in the table in service/ctl/tests/board_files.rs: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "table rows naming no board file: {stale:?}"
    );
    let mut sorted = listed.clone();
    sorted.sort();
    assert_eq!(sorted, listed, "keep the table sorted by file");
}

#[test]
fn every_board_file_validates_as_the_table_says() {
    let root = repo();
    let mut failures = Vec::new();
    for file in all_board_files() {
        let Some(row) = row_for(&file) else { continue };
        let got = validate(&root.join(&file));
        let ok = match (row.validate, &got) {
            (Validate::Pass, Ok(())) => true,
            (Validate::Refused(want), Err(have)) => want == have,
            _ => false,
        };
        if !ok {
            failures.push(format!(
                "{file}: table {:?}, validate says {got:?}",
                row.validate
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "validate disagrees with the table:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_daemons_check_agrees_with_the_table_for_every_board_file() {
    let root = repo();
    let mut failures = Vec::new();
    for file in all_board_files() {
        let Some(row) = row_for(&file) else { continue };
        let got = daemon(&root.join(&file));
        let ok = match (row.daemon, &got) {
            (Daemon::Pass, Ok(())) => true,
            (Daemon::Refused(want), Err(have)) => want == have,
            (Daemon::Machine(_), Ok(())) => true,
            (Daemon::Machine(prefix), Err(have)) => have.starts_with(prefix),
            _ => false,
        };
        if !ok {
            failures.push(format!(
                "{file}: table {:?}, the daemon says {got:?}",
                row.daemon
            ));
        }
        // Validation and the daemon's check share one settings check: where validate passes, the
        // daemon may refuse only for a reason of the machine, and never for a setting.
        if matches!(row.validate, Validate::Pass)
            && let Err(have) = &got
            && !matches!(row.daemon, Daemon::Machine(prefix) if have.starts_with(prefix))
        {
            failures.push(format!(
                "{file}: validate passes but the daemon refuses: {have}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the daemon disagrees with the table:\n{}",
        failures.join("\n")
    );
}
