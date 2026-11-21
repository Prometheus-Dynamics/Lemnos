//! The check on built boot partitions: a directory laid out as the firmware wants it passes, and
//! each way a bay went wrong on a real board (a padded gzip kernel, padded trees, a padded command
//! line, uppercase names, set file names, missing overlay maps) fails its own rule.

use crate::stream;
use crate::{Filters, Request, check};
use lemnos_slot_store::bootimage::Mem;
use lemnos_slot_store::fatimage::{Node, tree};

// --- built inputs ---------------------------------------------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A gzip member holding `plain` in one stored deflate block.
fn gzip(plain: &[u8]) -> Vec<u8> {
    let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    g.push(1); // BFINAL, stored
    let n = plain.len() as u16;
    g.extend_from_slice(&n.to_le_bytes());
    g.extend_from_slice(&(!n).to_le_bytes());
    g.extend_from_slice(plain);
    g.extend_from_slice(&crc32(plain).to_le_bytes());
    g.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    g
}

/// An arm64 Image's first bytes: the magic at 0x38.
fn image() -> Vec<u8> {
    let mut i = vec![0x11u8; 4096];
    i[0x38..0x3c].copy_from_slice(b"ARMd");
    i
}

/// A device tree from nodes (path, properties), the root first, each parent before its children.
/// One node of a built tree: its path and its properties.
type BuiltNode<'a> = (&'a str, &'a [(&'a str, &'a [u8])]);

fn fdt(nodes: &[BuiltNode<'_>]) -> Vec<u8> {
    let mut st: Vec<u8> = Vec::new();
    let mut strings: Vec<u8> = Vec::new();
    let mut open: Vec<String> = Vec::new();
    let word = |v: &mut Vec<u8>, w: u32| v.extend_from_slice(&w.to_be_bytes());
    let pad = |v: &mut Vec<u8>| {
        while !v.len().is_multiple_of(4) {
            v.push(0)
        }
    };
    for (path, props) in nodes {
        let parent = match path.rsplit_once('/') {
            Some(("", _)) if *path != "/" => "/".to_string(),
            Some((p, _)) if !p.is_empty() => p.to_string(),
            _ => String::new(),
        };
        while open.last().is_some_and(|o| *o != parent) {
            open.pop();
            word(&mut st, 2);
        }
        word(&mut st, 1);
        let name = if *path == "/" {
            ""
        } else {
            path.rsplit('/').next().unwrap()
        };
        st.extend_from_slice(name.as_bytes());
        st.push(0);
        pad(&mut st);
        for (k, v) in *props {
            let off = strings.len() as u32;
            strings.extend_from_slice(k.as_bytes());
            strings.push(0);
            word(&mut st, 3);
            word(&mut st, v.len() as u32);
            word(&mut st, off);
            st.extend_from_slice(v);
            pad(&mut st);
        }
        open.push(path.to_string());
    }
    for _ in &open {
        word(&mut st, 2);
    }
    word(&mut st, 9);
    let off_struct = 40 + 16;
    let off_strings = off_struct + st.len();
    let total = off_strings + strings.len();
    let mut out = Vec::new();
    for w in [
        0xd00d_feed,
        total as u32,
        off_struct as u32,
        off_strings as u32,
        40,
        17,
        16,
        0,
        strings.len() as u32,
        st.len() as u32,
    ] {
        word(&mut out, w);
    }
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&st);
    out.extend_from_slice(&strings);
    out
}

fn base_dtb() -> Vec<u8> {
    fdt(&[
        ("/", &[("model", b"t\0")]),
        ("/soc", &[]),
        ("/soc/gpio", &[]),
        ("/__symbols__", &[("gpio", b"/soc/gpio\0")]),
    ])
}

/// An overlay with one fragment at `target_path`, and a fixup naming `label`.
fn overlay(target_path: &str, label: &str) -> Vec<u8> {
    let tp = format!("{target_path}\0");
    let fix = b"/fragment@1:target:0\0";
    fdt(&[
        ("/", &[]),
        ("/fragment@0", &[("target-path", tp.as_bytes())]),
        ("/fragment@0/__overlay__", &[("status", b"okay\0")]),
        ("/fragment@1", &[("target", &[0xff, 0xff, 0xff, 0xff])]),
        ("/fragment@1/__overlay__", &[]),
        ("/__fixups__", &[(label, fix)]),
    ])
}

fn map_dtb() -> Vec<u8> {
    fdt(&[("/", &[]), ("/t", &[("x", b"\0")])])
}

/// A newc cpio archive holding one empty file and the trailer.
fn cpio() -> Vec<u8> {
    let mut out = Vec::new();
    for (name, mode) in [("init", 0o100755u32), ("TRAILER!!!", 0)] {
        let namesize = name.len() + 1;
        let h = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            1, mode, 0, 0, 1, 0, 0, 0, 0, 0, 0, namesize, 0
        );
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    out
}

/// What a directory holds; each test changes one thing.
struct Dir {
    config: String,
    kernel_name: &'static str,
    kernel: Vec<u8>,
    dtb: Vec<u8>,
    cmdline: Vec<u8>,
    cmdline_name: &'static str,
    initramfs: Vec<u8>,
    board_overlay: Vec<u8>,
    maps: bool,
    overlays_name: &'static str,
}

fn good() -> Dir {
    Dir {
        config: "arm_64bit=1\ninitramfs initramfs.cpio followkernel\n[none]\nkernel=x\n[all]\nos_prefix=slots/a/\ndtoverlay=board,p=1\n".to_string(),
        kernel_name: "kernel8.img",
        kernel: gzip(&image()),
        dtb: base_dtb(),
        cmdline: b"console=ttyS0 root=/dev/x rootwait\n".to_vec(),
        cmdline_name: "cmdline.txt",
        initramfs: cpio(),
        board_overlay: overlay("/soc", "gpio"),
        maps: true,
        overlays_name: "overlays",
    }
}

fn build(d: Dir) -> Vec<u8> {
    let mut ov = vec![
        Node::File {
            name: "README",
            data: b"overlays\n".to_vec(),
        },
        Node::File {
            name: "board.dtbo",
            data: d.board_overlay,
        },
    ];
    if d.maps {
        ov.push(Node::File {
            name: "overlay_map.dtb",
            data: map_dtb(),
        });
        ov.push(Node::File {
            name: "hat_map.dtb",
            data: map_dtb(),
        });
    }
    let config: &'static str = Box::leak(d.config.into_boxed_str());
    tree(&[
        Node::File {
            name: "config.txt",
            data: config.as_bytes().to_vec(),
        },
        Node::Dir {
            name: "slots",
            children: vec![Node::Dir {
                name: "a",
                children: vec![
                    Node::File {
                        name: d.kernel_name,
                        data: d.kernel,
                    },
                    Node::File {
                        name: "test-board.dtb",
                        data: d.dtb,
                    },
                    Node::File {
                        name: d.cmdline_name,
                        data: d.cmdline,
                    },
                    Node::File {
                        name: "initramfs.cpio",
                        data: d.initramfs,
                    },
                    Node::Dir {
                        name: d.overlays_name,
                        children: ov,
                    },
                ],
            }],
        },
    ])
}

fn request() -> Request {
    Request {
        config: "config.txt".to_string(),
        prefix: None,
        filters: Filters::default(),
        kernels: vec!["kernel_2712.img".to_string(), "kernel8.img".to_string()],
        dtbs: vec!["test-board.dtb".to_string()],
        needs_overlays: Vec::new(),
    }
}

/// The BAD lines of a check of `d`, as `rule: text`.
fn bad(d: Dir) -> Vec<String> {
    let report = check(&mut Mem(build(d)), &request());
    report
        .findings
        .iter()
        .filter(|f| !f.ok)
        .map(|f| format!("{}: {}", f.rule, f.text))
        .collect()
}

fn only(d: Dir, rule: &str, says: &str) {
    let b = bad(d);
    assert!(
        !b.is_empty(),
        "expected a BAD {rule} line saying {says:?}; all passed"
    );
    assert!(
        b.iter().all(|l| l.starts_with(&format!("{rule}: "))),
        "only {rule} should fail: {b:#?}"
    );
    assert!(
        b.iter().any(|l| l.contains(says)),
        "no BAD line says {says:?}: {b:#?}"
    );
}

// --- the views ------------------------------------------------------------------------------

#[test]
fn a_directory_laid_out_as_the_firmware_reads_it_passes_every_rule() {
    let report = check(&mut Mem(build(good())), &request());
    assert_eq!(report.prefix, "slots/a/");
    let b: Vec<_> = report.findings.iter().filter(|f| !f.ok).collect();
    assert!(b.is_empty(), "{b:#?}");
    for rule in [
        "names",
        "kernel",
        "dtb",
        "overlays",
        "cmdline",
        "initramfs",
        "case",
    ] {
        assert!(
            report.findings.iter().any(|f| f.rule == rule),
            "no {rule} line"
        );
    }
    assert!(report.pass());
}

#[test]
fn a_gzip_kernel_padded_with_zeros_fails_as_the_firmware_fails_it() {
    let mut d = good();
    let mut k = gzip(&image());
    k.resize(k.len() + 8192, 0);
    d.kernel = k;
    only(d, "kernel", "the file's last 4 bytes read 0");
    let mut d = good();
    d.kernel.resize(d.kernel.len() + 3, 0);
    only(d, "kernel", "bytes after it");
}

#[test]
fn a_gzip_kernel_with_a_wrong_crc_or_no_image_magic_fails() {
    let mut d = good();
    let n = d.kernel.len();
    d.kernel[n - 8] ^= 1;
    only(d, "kernel", "CRC-32 does not match");
    let mut d = good();
    d.kernel = gzip(&[0u8; 100]);
    only(d, "kernel", "magic missing");
}

#[test]
fn a_truncated_gzip_kernel_fails() {
    let mut d = good();
    let n = d.kernel.len();
    d.kernel.truncate(n - 20);
    only(d, "kernel", "no end mark");
}

#[test]
fn a_kernel_under_another_name_or_none_fails() {
    let mut d = good();
    d.kernel_name = "vmlinuz";
    only(d, "kernel", "none of kernel_2712.img, kernel8.img");
}

#[test]
fn a_device_tree_padded_past_its_totalsize_fails() {
    let mut d = good();
    d.dtb.resize(d.dtb.len() + 512, 0);
    only(d, "dtb", "bytes after the tree");
}

#[test]
fn a_device_tree_without_symbols_fails_and_so_do_the_overlays_on_it() {
    let mut d = good();
    d.dtb = fdt(&[("/", &[]), ("/soc", &[])]);
    let b = bad(d);
    assert!(
        b.iter()
            .any(|l| l.starts_with("dtb: ") && l.contains("no __symbols__")),
        "{b:#?}"
    );
    assert!(
        b.iter()
            .any(|l| l.starts_with("overlays: ") && l.contains("label gpio")),
        "{b:#?}"
    );
}

#[test]
fn an_overlay_padded_or_that_does_not_apply_fails() {
    let mut d = good();
    d.board_overlay.resize(d.board_overlay.len() + 64, 0);
    only(d, "overlays", "bytes after the tree");
    let mut d = good();
    d.board_overlay = overlay("/soc/none", "gpio");
    only(d, "overlays", "target-path /soc/none is not in the tree");
    let mut d = good();
    d.board_overlay = overlay("/soc", "uart9");
    only(
        d,
        "overlays",
        "label uart9 is not in the tree's __symbols__",
    );
}

#[test]
fn missing_overlay_maps_fail() {
    let mut d = good();
    d.maps = false;
    only(d, "overlays", "overlay_map.dtb: not there");
}

#[test]
fn a_command_line_with_padding_more_lines_or_too_long_fails() {
    let mut d = good();
    d.cmdline.resize(4096, b' ');
    let b = bad(d);
    assert!(b.iter().all(|l| l.starts_with("cmdline: ")), "{b:#?}");
    assert!(
        b.iter().any(|l| l.contains("blank bytes at its end")),
        "{b:#?}"
    );
    assert!(
        b.iter().any(|l| l.contains("4096 bytes, at most 1600")),
        "{b:#?}"
    );
    let mut d = good();
    d.cmdline = b"a=1\nb=2\n".to_vec();
    only(d, "cmdline", "more than one line");
    let mut d = good();
    d.cmdline = format!("{}\n", "x".repeat(1600)).into_bytes();
    only(d, "cmdline", "1601 bytes, at most 1600");
    let mut d = good();
    d.cmdline = format!("{}\n", "x".repeat(1599)).into_bytes();
    assert!(bad(d).is_empty());
}

#[test]
fn an_initramfs_padded_past_its_cpio_block_fails_and_one_block_of_zeros_does_not() {
    let mut d = good();
    d.initramfs.resize(1 << 20, 0);
    only(d, "initramfs", "bytes after it");
    let mut d = good();
    let n = d.initramfs.len();
    d.initramfs.resize(n.next_multiple_of(512), 0);
    assert!(bad(d).is_empty());
    let mut d = good();
    d.initramfs = b"garbage!".to_vec();
    only(d, "initramfs", "not cpio, gzip, zstd or xz");
}

#[test]
fn names_set_in_the_firmware_text_fail() {
    let mut d = good();
    d.config
        .push_str("kernel=kernel8.img\ncmdline=cmdline.txt\n");
    let b = bad(d);
    assert_eq!(b.len(), 2, "{b:#?}");
    assert!(b.iter().all(|l| l.starts_with("names: ")), "{b:#?}");
}

#[test]
fn names_stored_uppercase_fail_as_vfat_would_not_write_them() {
    let mut d = good();
    d.kernel_name = "KERNEL8.IMG";
    d.cmdline_name = "CMDLINE.TXT";
    d.overlays_name = "OVERLAYS";
    let b = bad(d);
    assert_eq!(b.len(), 3, "{b:#?}");
    assert!(b.iter().all(|l| l.starts_with("case: ")), "{b:#?}");
    assert!(
        b.iter()
            .any(|l| l.contains("stored as KERNEL8.IMG, not kernel8.img"))
    );
    assert!(b.iter().any(|l| l.contains("directory stored as OVERLAYS")));
}

#[test]
fn a_named_directory_is_read_as_the_default_one_would_be() {
    let mut d = good();
    d.config = d
        .config
        .replace("os_prefix=slots/a/", "os_prefix=slots/other/");
    let image = build(d);
    let mut req = request();
    assert!(!check(&mut Mem(image.clone()), &req).pass());
    req.prefix = Some("slots/a/".to_string());
    let r = check(&mut Mem(image), &req);
    assert!(r.pass(), "{:#?}", r.findings);
}

#[test]
fn the_trial_text_is_read_with_tryboot_on_and_unknown_filters_off() {
    let mut d = good();
    d.config = d.config.replace(
        "os_prefix=slots/a/",
        "os_prefix=slots/b/\n[tryboot]\nos_prefix=slots/a/\n[pi9]\nos_prefix=slots/c/\n[all]",
    );
    let image = build(d);
    let mut req = request();
    assert_eq!(check(&mut Mem(image.clone()), &req).prefix, "slots/b/");
    req.filters.trial = true;
    assert_eq!(check(&mut Mem(image.clone()), &req).prefix, "slots/a/");
    req.filters.on.push("pi9".to_string());
    assert_eq!(check(&mut Mem(image), &req).prefix, "slots/c/");
}

// --- the stream readers ---------------------------------------------------------------------

#[test]
fn a_gzip_member_with_name_and_comment_fields_is_read_to_its_end() {
    let plain = image();
    let mut g = gzip(&plain);
    g[3] = 0x08 | 0x10;
    let tail = g.split_off(10);
    g.extend_from_slice(b"name\0comment\0");
    g.extend_from_slice(&tail);
    let r = stream::gzip(&g).unwrap();
    assert_eq!(r.end, g.len());
    assert_eq!(r.plain_len, plain.len() as u64);
    assert!(r.crc_ok);
}

/// A zstd frame with one raw block of `n` bytes and a single-segment header.
fn zstd_frame(n: usize, checksum: bool) -> Vec<u8> {
    let mut f = 0xFD2F_B528u32.to_le_bytes().to_vec();
    f.push(0x20 | if checksum { 0x04 } else { 0 });
    f.push(n as u8);
    let h = 1u32 | ((n as u32) << 3);
    f.extend_from_slice(&h.to_le_bytes()[..3]);
    f.extend(std::iter::repeat_n(0x5a, n));
    if checksum {
        f.extend_from_slice(&[1, 2, 3, 4]);
    }
    f
}

#[test]
fn zstd_frames_are_walked_to_their_end_without_decompressing() {
    let mut z = zstd_frame(40, true);
    z.extend(zstd_frame(7, false));
    let end = z.len();
    assert_eq!(stream::zstd_end(&z).unwrap(), end);
    z.resize(end + 100, 0);
    assert_eq!(stream::zstd_end(&z).unwrap(), end);
    let mut short = zstd_frame(40, false);
    short.truncate(30);
    assert!(stream::zstd_end(&short).is_err());
}

#[test]
fn a_zstd_initramfs_with_padding_fails_and_an_exact_one_passes() {
    let mut d = good();
    d.initramfs = zstd_frame(64, true);
    assert!(bad(d).is_empty());
    let mut d = good();
    let mut z = zstd_frame(64, true);
    z.resize(4096, 0);
    d.initramfs = z;
    only(d, "initramfs", "zstd ends at byte");
}
