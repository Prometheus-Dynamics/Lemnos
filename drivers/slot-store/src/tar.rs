//! A tar read as it arrives: each member's bytes are passed on as they come, so a bundle is never
//! held whole. Plain files and directories only (ustar, with GNU long names and pax paths); a
//! link or a device in a bundle is refused.

/// What a member is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// Where the members go.
pub trait Sink {
    /// A member starts. `name` is relative, with no `.` or `..` part.
    fn begin(&mut self, name: &str, kind: Kind, size: u64) -> Result<(), String>;
    /// The next bytes of the current file.
    fn data(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// The current member is whole.
    fn end(&mut self) -> Result<(), String>;
}

const BLOCK: usize = 512;
/// The longest member name, and the most a long-name or pax record may hold.
const MAX_NAME: usize = 1024;
const MAX_RECORD: u64 = 64 * 1024;

#[derive(Debug)]
enum Body {
    File,
    LongName(Vec<u8>),
    Pax(Vec<u8>),
    Skip,
}

#[derive(Debug)]
enum State {
    Header,
    Body {
        left: u64,
        pad: usize,
        body: Body,
    },
    Pad(usize),
    /// After the end-of-archive block: only zeros may follow.
    Done,
}

/// The reader. Feed it the archive in pieces of any size.
#[derive(Debug)]
pub struct Tar {
    header: [u8; BLOCK],
    have: usize,
    state: State,
    long_name: Option<String>,
    pax_path: Option<String>,
}

impl Default for Tar {
    fn default() -> Self {
        Self {
            header: [0; BLOCK],
            have: 0,
            state: State::Header,
            long_name: None,
            pax_path: None,
        }
    }
}

impl Tar {
    /// Whether the archive has ended where it should: after its end block, between members.
    pub fn finished(&self) -> bool {
        matches!(self.state, State::Done)
    }

    pub fn feed(&mut self, mut bytes: &[u8], sink: &mut dyn Sink) -> Result<(), String> {
        while !bytes.is_empty() {
            match &mut self.state {
                State::Header => {
                    let take = (BLOCK - self.have).min(bytes.len());
                    self.header[self.have..self.have + take].copy_from_slice(&bytes[..take]);
                    self.have += take;
                    bytes = &bytes[take..];
                    if self.have == BLOCK {
                        self.have = 0;
                        self.header(sink)?;
                    }
                }
                State::Body { left, pad, body } => {
                    let take = usize::try_from(*left)
                        .unwrap_or(usize::MAX)
                        .min(bytes.len());
                    let (now, rest) = bytes.split_at(take);
                    bytes = rest;
                    *left -= take as u64;
                    match body {
                        Body::File => sink.data(now)?,
                        Body::LongName(text) | Body::Pax(text) => text.extend_from_slice(now),
                        Body::Skip => {}
                    }
                    if *left == 0 {
                        let pad = *pad;
                        let State::Body { body, .. } =
                            core::mem::replace(&mut self.state, State::Pad(pad))
                        else {
                            unreachable!()
                        };
                        self.finish(body, sink)?;
                        if pad == 0 {
                            self.state = State::Header;
                        }
                    }
                }
                State::Pad(left) => {
                    let take = (*left).min(bytes.len());
                    *left -= take;
                    bytes = &bytes[take..];
                    if *left == 0 {
                        self.state = State::Header;
                    }
                }
                State::Done => {
                    if bytes.iter().any(|&b| b != 0) {
                        return Err("bytes after the end of the bundle".into());
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, body: Body, sink: &mut dyn Sink) -> Result<(), String> {
        match body {
            Body::File => sink.end(),
            Body::LongName(text) => {
                let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
                let name = String::from_utf8(text[..end].to_vec())
                    .map_err(|_| "a member name that isn't UTF-8".to_string())?;
                self.long_name = Some(name);
                Ok(())
            }
            Body::Pax(text) => {
                self.pax_path = pax_path(&text)?;
                Ok(())
            }
            Body::Skip => Ok(()),
        }
    }

    fn header(&mut self, sink: &mut dyn Sink) -> Result<(), String> {
        let h = &self.header;
        if h.iter().all(|&b| b == 0) {
            self.state = State::Done;
            return Ok(());
        }
        let sum: u32 = h
            .iter()
            .enumerate()
            .map(|(at, &b)| {
                if (148..156).contains(&at) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        if octal(&h[148..156]) != Some(u64::from(sum)) {
            return Err("a member header's checksum is wrong".into());
        }
        let size = number(&h[124..136]).ok_or("a member's size can't be read")?;
        let pad = (BLOCK - (size % BLOCK as u64) as usize) % BLOCK;
        let typeflag = h[156];
        let body = match typeflag {
            b'L' | b'x' if size > MAX_RECORD => return Err("a member's header is too long".into()),
            b'L' => Body::LongName(Vec::new()),
            b'x' => Body::Pax(Vec::new()),
            b'g' => Body::Skip,
            b'0' | 0 | b'5' => {
                let raw = match (self.pax_path.take(), self.long_name.take()) {
                    (Some(path), _) | (None, Some(path)) => path,
                    (None, None) => header_name(h)?,
                };
                let kind = if typeflag == b'5' {
                    Kind::Dir
                } else {
                    Kind::File
                };
                let name = clean(&raw)?;
                if kind == Kind::Dir {
                    if !name.is_empty() {
                        sink.begin(&name, kind, 0)?;
                        sink.end()?;
                    }
                    // A directory's size is not followed by data in any tar we take.
                    self.state = State::Header;
                    return Ok(());
                }
                if name.is_empty() {
                    return Err("a file member with no name".into());
                }
                sink.begin(&name, kind, size)?;
                Body::File
            }
            other => {
                return Err(format!(
                    "the bundle has a member of type {:?}: only files and directories",
                    char::from(other)
                ));
            }
        };
        if size == 0 {
            self.state = State::Header;
            return self.finish(body, sink);
        }
        self.state = State::Body {
            left: size,
            pad,
            body,
        };
        Ok(())
    }
}

fn header_name(h: &[u8; BLOCK]) -> Result<String, String> {
    let field = |bytes: &[u8]| {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        core::str::from_utf8(&bytes[..end])
            .map(str::to_string)
            .map_err(|_| "a member name that isn't UTF-8".to_string())
    };
    let name = field(&h[0..100])?;
    // ustar: a prefix in 345..500.
    if &h[257..262] == b"ustar" {
        let prefix = field(&h[345..500])?;
        if !prefix.is_empty() {
            return Ok(format!("{prefix}/{name}"));
        }
    }
    Ok(name)
}

/// `path=` from a pax extended header, if it has one.
fn pax_path(text: &[u8]) -> Result<Option<String>, String> {
    let mut rest = text;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or("a pax record can't be read")?;
        let len: usize = core::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|n| n.parse().ok())
            .filter(|&n| n > space + 1 && n <= rest.len())
            .ok_or("a pax record can't be read")?;
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path =
                Some(String::from_utf8(value.to_vec()).map_err(|_| "a pax path that isn't UTF-8")?);
        }
        rest = &rest[len..];
    }
    Ok(path)
}

/// A member name as a relative path with no `.` or `..` part, and no trailing `/`.
fn clean(name: &str) -> Result<String, String> {
    if name.len() > MAX_NAME {
        return Err("a member name is too long".into());
    }
    if name.starts_with('/') {
        return Err(format!("the member {name} is an absolute path"));
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(format!("the member {name} reaches out of the bundle")),
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn octal(field: &[u8]) -> Option<u64> {
    let text = core::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c| c == ' ' || c == '\0');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// A size: octal, or base 256 when the first byte's top bit is set.
fn number(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        if field[0] & 0x40 != 0 {
            return None;
        }
        let mut value: u64 = u64::from(field[0] & 0x3f);
        for &b in &field[1..] {
            value = value.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(value);
    }
    octal(field)
}

/// Writes a tar: the same framing [`Tar`] reads. Plain files and directories, ustar headers with a GNU
/// long-name member when a name is over 99 bytes. Nothing in it depends on the clock or the machine
/// (owner 0, time 0, mode 0644 for a file and 0755 for a directory), so the same members give the
/// same bytes.
pub mod write {
    use super::BLOCK;

    /// The end of an archive: two zero blocks.
    pub const END: [u8; 2 * BLOCK] = [0; 2 * BLOCK];

    /// How many zero bytes follow a member of `size` bytes.
    pub fn padding(size: u64) -> usize {
        (BLOCK - (size % BLOCK as u64) as usize) % BLOCK
    }

    /// The header of `name` (`typeflag` `b'0'` a file, `b'5'` a directory) of `size` bytes, long-name
    /// member first when the name needs it.
    pub fn header(name: &str, typeflag: u8, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        let short = if name.len() > 99 {
            let mut long = name.as_bytes().to_vec();
            long.push(0);
            out.extend(header("././@LongLink", b'L', long.len() as u64));
            out.extend(&long);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
            &name[..99]
        } else {
            name
        };
        let mut h = [0u8; BLOCK];
        h[..short.len()].copy_from_slice(short.as_bytes());
        let mode: &[u8; 7] = if typeflag == b'5' {
            b"0000755"
        } else {
            b"0000644"
        };
        h[100..107].copy_from_slice(mode);
        if size < 1 << 33 {
            h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        } else {
            // Base 256: the top bit of the first byte, then the value, big-endian.
            h[124] = 0x80;
            h[128..136].copy_from_slice(&size.to_be_bytes());
        }
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend(h);
        out
    }

    /// A whole archive of `files` (a name ending in `/` is a directory), ended as tar ends one.
    pub fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, bytes) in files {
            if name.ends_with('/') {
                out.extend(header(name, b'5', 0));
                continue;
            }
            out.extend(header(name, b'0', bytes.len() as u64));
            out.extend(*bytes);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend(END);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub use super::write::{archive, header};

    #[derive(Debug, Default)]
    struct Seen(Vec<(String, Kind, Vec<u8>, bool)>);

    impl Sink for Seen {
        fn begin(&mut self, name: &str, kind: Kind, _: u64) -> Result<(), String> {
            self.0.push((name.to_string(), kind, Vec::new(), false));
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.0.last_mut().unwrap().2.extend_from_slice(bytes);
            Ok(())
        }
        fn end(&mut self) -> Result<(), String> {
            self.0.last_mut().unwrap().3 = true;
            Ok(())
        }
    }

    fn read_in(tar: &[u8], piece: usize) -> Result<(Seen, bool), String> {
        let mut reader = Tar::default();
        let mut seen = Seen::default();
        for chunk in tar.chunks(piece) {
            reader.feed(chunk, &mut seen)?;
        }
        Ok((seen, reader.finished()))
    }

    #[test]
    fn members_come_whole_whatever_the_pieces() {
        let long = format!("boot/overlays/{}.dtbo", "x".repeat(120));
        let big = vec![7u8; 1500];
        let tar = archive(&[
            ("./slot.json", b"{}"),
            ("boot/", b""),
            (&long, b"dt"),
            ("rootfs.erofs", &big),
            ("empty", b""),
        ]);
        for piece in [1, 7, 512, 513, 100_000] {
            let (seen, finished) = read_in(&tar, piece).unwrap();
            assert!(finished);
            let names: Vec<_> = seen.0.iter().map(|s| (s.0.as_str(), s.1, s.3)).collect();
            assert_eq!(
                names,
                [
                    ("slot.json", Kind::File, true),
                    ("boot", Kind::Dir, true),
                    (long.as_str(), Kind::File, true),
                    ("rootfs.erofs", Kind::File, true),
                    ("empty", Kind::File, true),
                ]
            );
            assert_eq!(seen.0[0].2, b"{}");
            assert_eq!(seen.0[3].2, big);
        }
    }

    #[test]
    fn a_pax_path_names_the_next_member() {
        let record = b"29 path=boot/a-long-pax-name\n";
        let mut tar = header("PaxHeaders/x", b'x', record.len() as u64);
        tar.extend(record);
        tar.resize(tar.len().div_ceil(BLOCK) * BLOCK, 0);
        tar.extend(archive(&[("short", b"z")]));
        let (seen, _) = read_in(&tar, 64).unwrap();
        assert_eq!(seen.0[0].0, "boot/a-long-pax-name");
    }

    #[test]
    fn what_could_reach_outside_or_isnt_a_file_is_refused() {
        for name in ["../x", "a/../../x", "/etc/x"] {
            assert!(read_in(&archive(&[(name, b"1")]), 512).is_err(), "{name}");
        }
        let mut link = header("l", b'2', 0);
        link.extend([0; 1024]);
        assert!(read_in(&link, 512).unwrap_err().contains("type '2'"));
        let mut bad = archive(&[("a", b"1")]);
        bad[0] ^= 1;
        assert!(read_in(&bad, 512).unwrap_err().contains("checksum"));
        let mut after = archive(&[("a", b"1")]);
        after.push(1);
        assert!(read_in(&after, 512).is_err());
    }

    #[test]
    fn a_tar_cut_short_has_not_finished() {
        let tar = archive(&[("a", &[1; 600])]);
        let (_, finished) = read_in(&tar[..700], 512).unwrap();
        assert!(!finished);
    }
}
