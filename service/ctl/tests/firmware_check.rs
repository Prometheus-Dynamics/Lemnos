//! `lemnos-ctl firmware-check`: the verdict lines and the exit, on built boot partitions.

use lemnos_slot_store::fatimage::{Node, tree};

fn image(kernel: Vec<u8>) -> Vec<u8> {
    tree(&[
        Node::File {
            name: "config.txt",
            data: b"os_prefix=slots/a/\n".to_vec(),
        },
        Node::Dir {
            name: "slots",
            children: vec![Node::Dir {
                name: "a",
                children: vec![Node::File {
                    name: "kernel8.img",
                    data: kernel,
                }],
            }],
        },
    ])
}

fn run(img: &[u8], extra: &[&str]) -> (Result<(), String>, String) {
    // One file per call, named for the process and the call, removed before returning: the gate
    // refuses a test that leaves anything in TMPDIR.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = lemnos_scratch::Scratch::adopt(
        std::env::temp_dir().join(format!("fwcheck-{}-{n}.img", std::process::id())),
    );
    std::fs::write(&path, img).unwrap();
    let mut args = vec!["firmware-check".to_string(), path.display().to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&args, &mut out);
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn every_rule_prints_a_line_and_a_failing_one_exits_non_zero_after_all_of_them() {
    let mut k = vec![0u8; 256];
    k[0x38..0x3c].copy_from_slice(b"ARMd");
    let (r, out) = run(&image(k), &["--dtb", "board.dtb"]);
    assert!(r.is_err(), "{out}");
    assert!(
        out.starts_with("view config.txt (prefix \"slots/a/\")"),
        "{out}"
    );
    assert!(
        out.contains("ok  kernel: slots/a/kernel8.img: uncompressed"),
        "{out}"
    );
    assert!(
        out.contains("BAD dtb: slots/a/board.dtb: not there"),
        "{out}"
    );
    assert!(
        out.contains("BAD cmdline: slots/a/cmdline.txt: not there"),
        "{out}"
    );
    assert!(
        out.trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("firmware-layout rule 1: FAIL"),
        "{out}"
    );
}

#[test]
fn the_two_views_cannot_be_asked_for_together_and_an_image_is_needed() {
    let (r, _) = run(
        &image(vec![0; 64]),
        &["--trial", "--dir", "x/", "--dtb", "b.dtb"],
    );
    assert!(r.unwrap_err().contains("--trial and --dir"));
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&["firmware-check".to_string()], &mut out);
    assert!(r.unwrap_err().contains("needs an IMAGE"));
}
