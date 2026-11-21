//! Stand-ins shaped as the firmware reads them, for the tests of the check and of the image
//! builder: a gzip arm64 kernel, a newc cpio initramfs, a device tree with `__symbols__`, an overlay
//! that applies to it, and the overlay and HAT maps. Each is exact: the check passes it, and each
//! way a real bay went wrong (padding, a wrong CRC, no magic) fails it.
#![allow(clippy::missing_panics_doc)]

/// The CRC-32 a gzip trailer carries.
pub fn crc32(bytes: &[u8]) -> u32 {
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

/// A gzip member holding `plain` in stored deflate blocks (no compressor: the check reads any
/// deflate stream, and this one is exact to the byte).
pub fn gzip(plain: &[u8]) -> Vec<u8> {
    let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    let mut chunks = plain.chunks(0xFFFF).peekable();
    if chunks.peek().is_none() {
        g.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(chunk) = chunks.next() {
        g.push(u8::from(chunks.peek().is_none())); // BFINAL on the last, stored
        let n = chunk.len() as u16;
        g.extend_from_slice(&n.to_le_bytes());
        g.extend_from_slice(&(!n).to_le_bytes());
        g.extend_from_slice(chunk);
    }
    g.extend_from_slice(&crc32(plain).to_le_bytes());
    g.extend_from_slice(&(plain.len() as u32).to_le_bytes());
    g
}

/// An arm64 Image's first bytes: the magic at 0x38.
pub fn image() -> Vec<u8> {
    arm64_image(4096)
}

/// An arm64 Image of `len` bytes (at least 64): the magic at 0x38, a fill after it.
pub fn arm64_image(len: usize) -> Vec<u8> {
    let mut i = vec![0x11u8; len.max(64)];
    i[0x38..0x3c].copy_from_slice(b"ARMd");
    i
}

/// A kernel as the firmware takes it: an arm64 Image of `len` bytes, gzip-wrapped.
pub fn kernel(len: usize) -> Vec<u8> {
    gzip(&arm64_image(len))
}

/// A device tree from nodes (path, properties), the root first, each parent before its children.
/// One node of a built tree: its path and its properties.
pub type BuiltNode<'a> = (&'a str, &'a [(&'a str, &'a [u8])]);

pub fn fdt(nodes: &[BuiltNode<'_>]) -> Vec<u8> {
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

pub fn base_dtb() -> Vec<u8> {
    fdt(&[
        ("/", &[("model", b"t\0")]),
        ("/soc", &[]),
        ("/soc/gpio", &[]),
        ("/__symbols__", &[("gpio", b"/soc/gpio\0")]),
    ])
}

/// An overlay with one fragment at `target_path`, and a fixup naming `label`.
pub fn overlay(target_path: &str, label: &str) -> Vec<u8> {
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

pub fn map_dtb() -> Vec<u8> {
    fdt(&[("/", &[]), ("/t", &[("x", b"\0")])])
}

/// A newc cpio archive holding one empty file and the trailer.
pub fn cpio() -> Vec<u8> {
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
