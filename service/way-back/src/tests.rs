use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use super::{Ask, DEFAULT_DEVICE, Firmware, Mailbox, USAGE, parse, run};

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn the_one_way_back_is_rpiboot_on_the_default_node_or_the_one_named() {
    assert_eq!(
        parse(&words("rpiboot")),
        Ok(Ask {
            device: PathBuf::from(DEFAULT_DEVICE)
        })
    );
    assert_eq!(
        parse(&words("rpiboot --device /dev/other")),
        Ok(Ask {
            device: PathBuf::from("/dev/other")
        })
    );
}

#[test]
fn anything_else_is_refused_with_the_usage() {
    for line in [
        "",
        "--device /dev/vcio",
        "rpiboot --device",
        "rpiboot --other x",
        "rpiboot extra",
    ] {
        let why = parse(&words(line)).unwrap_err();
        assert!(why.ends_with(USAGE), "{line:?}: {why}");
    }
    let other = parse(&words("tryboot")).unwrap_err();
    assert!(
        other.contains("\"tryboot\" is not a way back") && other.ends_with(USAGE),
        "{other}"
    );
}

/// Records what it was asked and answers as told.
struct Stand {
    asked: RefCell<Vec<PathBuf>>,
    answer: Option<io::ErrorKind>,
}

impl Firmware for Stand {
    fn request_rpiboot(&self, device: &Path) -> io::Result<()> {
        self.asked.borrow_mut().push(device.to_path_buf());
        self.answer
            .map_or(Ok(()), |kind| Err(io::Error::from(kind)))
    }
}

#[test]
fn one_request_is_sent_and_its_answer_is_the_result() {
    let ask = Ask {
        device: PathBuf::from("/dev/vcio"),
    };
    let taken = Stand {
        asked: RefCell::new(Vec::new()),
        answer: None,
    };
    let line = run(&ask, &taken).unwrap();
    assert!(
        line.contains("took RPIBOOT for the next boot") && line.contains("/dev/vcio"),
        "{line}"
    );
    assert_eq!(
        taken.asked.borrow().as_slice(),
        [PathBuf::from("/dev/vcio")]
    );

    let refused = Stand {
        asked: RefCell::new(Vec::new()),
        answer: Some(io::ErrorKind::NotFound),
    };
    let why = run(&ask, &refused).unwrap_err();
    assert!(
        why.starts_with("the way back is not armed: RPIBOOT through /dev/vcio"),
        "{why}"
    );
    assert_eq!(
        refused.asked.borrow().len(),
        1,
        "one request, never retried here"
    );
}

/// The real mailbox on a node that is no mailbox: an ordinary file answers the ioctl with an error,
/// and a missing node fails at the open. Neither is taken for a yes.
#[test]
fn a_node_that_is_not_the_mailbox_is_never_taken_for_a_yes() {
    let dir = std::env::temp_dir().join(format!("lemnos-way-back-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("vcio");
    std::fs::write(&file, [0u8; 32]).unwrap();
    for device in [file.clone(), dir.join("missing")] {
        let why = run(
            &Ask {
                device: device.clone(),
            },
            &Mailbox,
        )
        .unwrap_err();
        assert!(why.contains(&device.display().to_string()), "{why}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
