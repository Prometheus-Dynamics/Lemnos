//! A flattened device tree, read: its header, every node by path and its properties.

const MAGIC: u32 = 0xd00d_feed;
const BEGIN_NODE: u32 = 1;
const END_NODE: u32 = 2;
const PROP: u32 = 3;
const NOP: u32 = 4;
const END: u32 = 9;

/// A node: its absolute path and its properties, in order.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) path: String,
    pub(crate) props: Vec<(String, Vec<u8>)>,
}

/// A device tree, read.
#[derive(Debug, Clone)]
pub(crate) struct Tree {
    /// The header's `totalsize`.
    pub(crate) total: usize,
    pub(crate) nodes: Vec<Node>,
}

fn be32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("the tree ends inside a word at byte {at}"))
}

fn cstr(b: &[u8], at: usize) -> Result<(&str, usize), String> {
    let rest = b.get(at..).ok_or("a name starts past the end")?;
    let n = rest
        .iter()
        .position(|&c| c == 0)
        .ok_or("a name has no terminator")?;
    let s = std::str::from_utf8(&rest[..n]).map_err(|_| "a name is not UTF-8".to_string())?;
    Ok((s, n + 1))
}

/// Reads `data` as a flattened device tree. The structure is read within `totalsize`, and
/// `totalsize` itself is not compared with the file here (the caller does).
pub(crate) fn read(data: &[u8]) -> Result<Tree, String> {
    if be32(data, 0)? != MAGIC {
        return Err("no FDT magic".to_string());
    }
    let total = be32(data, 4)? as usize;
    if total > data.len() {
        return Err(format!(
            "totalsize {total} is past the end of the file ({} bytes)",
            data.len()
        ));
    }
    let data = &data[..total];
    let off_struct = be32(data, 8)? as usize;
    let off_strings = be32(data, 12)? as usize;
    let size_strings = be32(data, 32)? as usize;
    let strings = data
        .get(off_strings..off_strings + size_strings)
        .ok_or("the strings block is outside totalsize")?;
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut at = off_struct;
    loop {
        let token = be32(data, at)?;
        at += 4;
        match token {
            BEGIN_NODE => {
                let (name, n) = cstr(data, at)?;
                at = (at + n).next_multiple_of(4);
                let path = match stack.last() {
                    None => "/".to_string(),
                    Some(&p) if nodes[p].path == "/" => format!("/{name}"),
                    Some(&p) => format!("{}/{name}", nodes[p].path),
                };
                nodes.push(Node {
                    path,
                    props: Vec::new(),
                });
                stack.push(nodes.len() - 1);
            }
            END_NODE => {
                stack.pop().ok_or("an end-node with no node open")?;
            }
            PROP => {
                let len = be32(data, at)? as usize;
                let nameoff = be32(data, at + 4)? as usize;
                at += 8;
                let value = data
                    .get(at..at + len)
                    .ok_or("a property runs past totalsize")?
                    .to_vec();
                at = (at + len).next_multiple_of(4);
                let (name, _) = cstr(strings, nameoff)?;
                let &open = stack.last().ok_or("a property outside any node")?;
                nodes[open].props.push((name.to_string(), value));
            }
            NOP => {}
            END => break,
            other => {
                return Err(format!(
                    "unknown structure token {other} at byte {}",
                    at - 4
                ));
            }
        }
    }
    if !stack.is_empty() {
        return Err("the structure ends with nodes still open".to_string());
    }
    Ok(Tree { total, nodes })
}

impl Tree {
    pub(crate) fn node(&self, path: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.path == path)
    }

    /// The property names of `path` (for `__symbols__` and `__fixups__`, the labels).
    pub(crate) fn names(&self, path: &str) -> Vec<&str> {
        self.node(path)
            .map(|n| n.props.iter().map(|(k, _)| k.as_str()).collect())
            .unwrap_or_default()
    }

    /// Whether `path` names a node, directly or through `/aliases` (a target path without a
    /// leading `/` is an alias, optionally followed by a sub-path).
    pub(crate) fn resolves(&self, path: &str) -> bool {
        if path.starts_with('/') {
            return self.node(path).is_some();
        }
        let (alias, rest) = path.split_once('/').unwrap_or((path, ""));
        let Some(aliases) = self.node("/aliases") else {
            return false;
        };
        let Some((_, v)) = aliases.props.iter().find(|(k, _)| k == alias) else {
            return false;
        };
        let base = String::from_utf8_lossy(v.strip_suffix(&[0]).unwrap_or(v)).to_string();
        let full = if rest.is_empty() {
            base
        } else {
            format!("{base}/{rest}")
        };
        self.node(&full).is_some()
    }
}

/// Why `overlay` would not apply to `base`: each fragment's `target-path` must name a node of the
/// base, and every label the overlay's `__fixups__` lists must be in the base's `__symbols__`.
/// Empty when it applies.
pub(crate) fn overlay_misses(overlay: &Tree, base: &Tree) -> Vec<String> {
    let mut misses = Vec::new();
    let symbols = base.names("/__symbols__");
    for label in overlay.names("/__fixups__") {
        if !symbols.contains(&label) {
            misses.push(format!("label {label} is not in the tree's __symbols__"));
        }
    }
    for n in &overlay.nodes {
        let Some(frag) = n.path.strip_suffix("/__overlay__") else {
            continue;
        };
        let Some(f) = overlay.node(frag) else {
            continue;
        };
        if let Some((_, v)) = f.props.iter().find(|(k, _)| k == "target-path") {
            let p = String::from_utf8_lossy(v.strip_suffix(&[0]).unwrap_or(v)).to_string();
            if !base.resolves(&p) {
                misses.push(format!("{frag}: target-path {p} is not in the tree"));
            }
        }
    }
    misses
}
