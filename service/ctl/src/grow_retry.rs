//! `lemnos-ctl grow-retry DATA`: clears a grow the boot refused because its last attempt never returned
//! (`grow-stalled`), so the next boot tries the grow once more. Run as root on the board, against the
//! data partition's mount point (`DATA`, the directory that holds `board/`). It removes the state file
//! and nothing else, and only when the file says that refusal: any other state is named and left as it is.
//!
//! The boot never retries that attempt on its own (a reset loop on a grow that stalls would be the
//! watchdog's failure, not the grow's), so this is the one explicit request that tries it again.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;

use lemnos_slot_store::grow::{self, STALLED, State};

/// # Errors
///
/// No state file, one that does not decode, one that is not the stalled refusal, or a file that can't
/// be removed. Each is named, and nothing is removed unless it is the refusal.
pub fn run(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    let [data] = args else {
        return Err("usage: lemnos-ctl grow-retry DATA (the data partition's mount point)".into());
    };
    let path = Path::new(data)
        .join("board")
        .join("slots")
        .join(grow::STATE_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(format!(
                "{}: no grow state, so no refusal to clear",
                path.display()
            ));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    match grow::decode_state(&text) {
        Some(State::Refused { state, reason }) if state == STALLED => {
            fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            writeln!(
                out,
                "grow-retry: cleared ({reason}); the next boot tries the grow again"
            )
            .map_err(|e| e.to_string())
        }
        Some(other) => Err(format!(
            "{}: the grow is not refused as stalled ({other:?}): nothing was cleared",
            path.display()
        )),
        None => Err(format!(
            "{}: the grow state does not decode: nothing was cleared",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(name: &str, state: Option<&str>) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lemnos-ctl-grow-retry-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("board/slots")).unwrap();
        if let Some(text) = state {
            fs::write(root.join("board/slots/grow-state"), text).unwrap();
        }
        root
    }

    #[test]
    fn the_stalled_refusal_is_cleared_for_one_more_try() {
        let root = data(
            "cleared",
            Some("refused grow-stalled refused: the last grow did not return (boot b1)\n"),
        );
        let mut out = Vec::new();
        run(&[root.display().to_string()], &mut out).unwrap();
        assert!(!root.join("board/slots/grow-state").exists());
        assert!(String::from_utf8_lossy(&out).contains("the next boot tries the grow again"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn any_other_state_is_named_and_left_alone() {
        let root = data("other", Some("refused data-read-only data-read-only: ro\n"));
        let error = run(&[root.display().to_string()], &mut Vec::new()).unwrap_err();
        assert!(error.contains("not refused as stalled"), "{error}");
        assert!(root.join("board/slots/grow-state").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_state_is_named() {
        let root = data("none", None);
        let error = run(&[root.display().to_string()], &mut Vec::new()).unwrap_err();
        assert!(error.contains("no refusal to clear"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }
}
