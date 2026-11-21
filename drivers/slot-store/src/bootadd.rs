//! Adds this system's boot files to a dump of a boot partition another system boots from, beside
//! that system's own files, so a slot can be started once by the firmware's tryboot without anything
//! the existing system boots from being touched (`lemnos-ctl boot-partition --add-to`).
//!
//! The files are the ones the image builder emits ([`crate::bootimage`]'s own tree, so the two
//! cannot drift), except `default.txt` and `config.txt`: those are the existing system's, and are
//! never created, replaced or resized here. `trial.txt` and `tryboot.txt` are the existing system's
//! too when it has them; they are replaced only when the caller asks ([`Request::replace_trial`]).
//! The slot is installed into one bay set by the installer's write part ([`bays::stage_with`]), which
//! moves no pointer.
//!
//! Everything is done on a copy in memory. The result is given back only when the audit of
//! [`crate::fatadd`] passes, the volume is sound when read again with every earlier entry where it
//! was, and the image builder's own verification shows exactly the lines expected.

use crate::bayfat;
use crate::bays::{self, Done, INFO_NAME, POINTER_BYTES, Protected, SETS};
use crate::bootfat::{self, ReadAt, Refusal};
use crate::bootimage::{self, Family, Finding, Mem, Parts};
use crate::fatadd::{self, Added, Alloc, Change, DirAt, Editor, FreeMap, Replaced};
use crate::fatimage::Node;
use crate::firmware;
use crate::twocopy::{SECTOR, Sector, Sectors};

/// The root files of the existing system that are never created, replaced or resized.
pub const KEPT: [&str; 2] = ["default.txt", "config.txt"];
/// The root files the trial needs in this system's form, replaced only on request.
pub const TRIAL_PAIR: [&str; 2] = ["trial.txt", "tryboot.txt"];

/// What to add.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The slot installed.
    pub slot: &'a str,
    /// The bay set it is installed into. `trial.txt`, where this run writes it, names this set.
    pub set: u8,
    /// Replace an existing `trial.txt` and `tryboot.txt` that are not in this system's form.
    pub replace_trial: bool,
    /// The set's members: the five roles, then the board's firmware overlays.
    pub payloads: &'a [&'a [u8]],
}

/// What a run did to the dump's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// None of the files were there: all were added, and the slot installed.
    Added,
    /// Every file was there already, placed by an earlier run: the slot was installed into an
    /// empty set and no file was added.
    InstalledOnly,
}

/// The result of an add.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The dump with the files added.
    pub image: Vec<u8>,
    /// What the run did.
    pub mode: Mode,
    /// Every changed range of sectors, in sector order.
    pub changes: Vec<Change>,
    /// The undo file: the bytes every changed range had before ([`fatadd::apply_undo`]).
    pub undo: Vec<u8>,
    /// Every file and directory added, in the order they were added.
    pub added: Vec<Added>,
    /// The files replaced (`trial.txt`, `tryboot.txt`), with what they were.
    pub replaced: Vec<Replaced>,
    /// The files of [`TRIAL_PAIR`] that were there, are not in this system's form, and were left.
    pub left: Vec<String>,
    /// The verification of the result, line by line.
    pub findings: Vec<Finding>,
    /// The files whose `BAD` line in the verification is expected: one line each, and no other.
    pub expected_bad: Vec<String>,
    /// The free clusters before.
    pub free_before: FreeMap,
    /// The free clusters after.
    pub free_after: FreeMap,
}

/// A dump read in place: never written.
struct View<'a>(&'a [u8]);

impl ReadAt for View<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let at = offset as usize;
        let from = self
            .0
            .get(at..at + buf.len())
            .ok_or_else(|| std::io::Error::other("read past the end of the dump"))?;
        buf.copy_from_slice(from);
        Ok(())
    }
}

impl Sectors for View<'_> {
    fn read(&mut self, lba: u64, buf: &mut Sector) -> std::io::Result<()> {
        self.read_at(lba * SECTOR as u64, buf)
    }

    fn write(&mut self, _lba: u64, _buf: &Sector) -> std::io::Result<()> {
        Err(std::io::Error::other("the original dump is never written"))
    }
}

/// The files of a tree, `(path, bytes)`, in the builder's order.
fn flatten<'n>(nodes: &'n [Node], dir: &str, out: &mut Vec<(String, &'n [u8])>) {
    for node in nodes {
        match node {
            Node::File { name, data } => out.push((join(dir, name), data.as_slice())),
            Node::Dir { name, children } | Node::DirOf { name, children, .. } => {
                flatten(children, &join(dir, name), out)
            }
            // A set's region is sized by the volume's clusters: it is added, not listed.
            Node::Region { .. } => {}
        }
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The tree the image builder emits for `parts`, with `trial.txt` naming `set`: the files this adds,
/// before the existing system's are left out.
fn our_tree(parts: &Parts, set: u8) -> Result<Vec<Node>, String> {
    let mut nodes = bootimage::tree(parts);
    let pointer = firmware::pointer_file(&bays::set_name(set))
        .ok_or_else(|| format!("bay set {set} has no pointer text"))?;
    for node in &mut nodes {
        if let Node::File { name, data } = node
            && *name == bays::TRIAL_POINTER
        {
            *data = pointer.to_vec();
        }
    }
    Ok(nodes)
}

/// The paths this adds to a volume that has none of them, with their sizes, in the order they are
/// added: the builder's files for `parts` without [`KEPT`]. Directories are not listed.
pub fn file_list(parts: &Parts) -> Result<Vec<(String, u64)>, String> {
    let nodes = our_tree(parts, 0)?;
    let mut files = Vec::new();
    flatten(&nodes, "", &mut files);
    Ok(files
        .into_iter()
        .filter(|(path, _)| !KEPT.contains(&path.as_str()))
        .map(|(path, bytes)| (path, bytes.len() as u64))
        .collect())
}

/// Adds this system's files for `parts` to `dump` and installs `req.slot` into bay set `req.set`.
/// The allocation rule is [`fatadd::first_fit`]. Nothing is given back unless every check passes;
/// each refusal ends in `nothing was written`.
pub fn add_to(parts: &Parts, dump: &[u8], req: &Request<'_>) -> Result<Outcome, String> {
    add_to_with(parts, dump, req, &fatadd::first_fit)
}

/// [`add_to`] with the allocator given: the tests pass a broken one to show the audit refuses it.
pub fn add_to_with(
    parts: &Parts,
    dump: &[u8],
    req: &Request<'_>,
    alloc: Alloc<'_>,
) -> Result<Outcome, String> {
    if parts.family != Family::Tryboot {
        return Err(
            "--add-to is for the Pi firmware form (the \"tryboot\" record): nothing was written"
                .to_string(),
        );
    }
    bays::check_install(&parts.layout, req.set, req.slot, req.payloads)
        .map_err(|e| e.to_string())?;
    let nodes = our_tree(parts, req.set)?;
    let mut files = Vec::new();
    flatten(&nodes, "", &mut files);

    let mut ed = Editor::new(dump, alloc)?;
    let free_before = ed.survey.free_map();
    let mut view = View(dump);

    // Which of our files the dump already holds. All or none: a dump that holds some is refused.
    let mut present: Vec<&str> = Vec::new();
    let mut absent: Vec<&str> = Vec::new();
    for (path, bytes) in &files {
        let name = path.as_str();
        if KEPT.contains(&name) || TRIAL_PAIR.contains(&name) {
            continue;
        }
        match bootfat::locate_path(&mut view, path, bytes.len() as u64) {
            Ok(_) => present.push(name),
            Err(Refusal::Missing) => absent.push(name),
            Err(e) => {
                return Err(format!(
                    "{path} is already on the volume and is not a file of ours ({e}): nothing was written"
                ));
            }
        }
    }
    let mode = match (present.first(), absent.first()) {
        (None, _) => Mode::Added,
        (Some(_), None) => Mode::InstalledOnly,
        (Some(have), Some(lack)) => {
            return Err(format!(
                "{have} is already on the volume and {lack} is not: the dump holds a part of our files, which no run of this tool leaves: nothing was written"
            ));
        }
    };

    // A set directory is ours only with its record: a fresh add never moves into one it finds.
    if mode == Mode::Added {
        for set in 0..bays::SETS {
            let dir = bays::set_dir(set);
            if ed
                .survey
                .entries
                .iter()
                .any(|e| e.path.eq_ignore_ascii_case(&dir))
            {
                return Err(format!(
                    "{dir} is already on the volume and is not a bay set of ours (it holds no {INFO_NAME}): nothing was written"
                ));
            }
        }
    }

    // The trial pair: added when absent, left when it is already what we would write, replaced on
    // request, and otherwise left as the existing system's.
    let mut add_pair: Vec<&str> = Vec::new();
    let mut replace: Vec<(&str, &[u8])> = Vec::new();
    let mut left: Vec<String> = Vec::new();
    let mut expected_bad: Vec<String> = KEPT.iter().map(|s| s.to_string()).collect();
    for name in TRIAL_PAIR {
        let Some((_, want)) = files.iter().find(|(path, _)| path == name) else {
            continue;
        };
        if !ed
            .survey
            .entries
            .iter()
            .any(|e| e.path.eq_ignore_ascii_case(name))
        {
            add_pair.push(name);
            continue;
        }
        let ours = bootfat::locate_path(&mut view, name, want.len() as u64)
            .ok()
            .and_then(|_| bootfat::read_file(&mut view, name, want.len() as u64).ok())
            .is_some_and(|have| have == *want);
        if ours {
            continue;
        }
        if req.replace_trial {
            replace.push((name, want));
            continue;
        }
        left.push(name.to_string());
        // What the verification says of a file left in place: a pointer is good when it is two
        // sectors holding a whole line (naming any set), the text only when it is the generated text.
        let reads = name == bays::TRIAL_POINTER
            && bootfat::locate_path(&mut view, name, POINTER_BYTES)
                .ok()
                .and_then(|f| bays::read_pointer(&mut view, f.first_lba).ok().flatten())
                .is_some();
        if !reads {
            expected_bad.push(name.to_string());
        }
    }

    match mode {
        Mode::Added => {
            let keep = |path: &str| {
                !KEPT.contains(&path) && (!TRIAL_PAIR.contains(&path) || add_pair.contains(&path))
            };
            ed.add_nodes(&DirAt::Root, "", &nodes, &keep)?;
        }
        Mode::InstalledOnly => {
            // The sets must be this layout's: a dump made for another layout version is not written.
            if let Some(why) = bays::records_version(&mut view)? {
                return Err(why);
            }
            // The set must be empty, and the slot in no set: this installs, it never reinstalls.
            let sets =
                bays::place_sets(&mut view).map_err(|e| format!("{e}: nothing was written"))?;
            let mut region = None;
            for set in 0..SETS {
                let info = bays::read_info(&mut view, sets[set as usize])
                    .map_err(|e| format!("bay{set}: {e}: nothing was written"))?;
                match info {
                    Some(bays::Info {
                        slot: Some(held), ..
                    }) if set == req.set => {
                        return Err(format!(
                            "bay{set} already holds the slot {held}: --add-to installs into an empty bay set only: nothing was written"
                        ));
                    }
                    Some(bays::Info {
                        slot: Some(held), ..
                    }) if held == req.slot => {
                        return Err(format!(
                            "the slot {} is already in bay{set}: nothing was written",
                            req.slot
                        ));
                    }
                    Some(i) if set == req.set => region = Some(i.region),
                    _ => {}
                }
            }
            let region = region.ok_or_else(|| {
                format!(
                    "bay{}'s record names no region: the dump's bays were not made for layout version {}: nothing was written",
                    req.set,
                    bays::LAYOUT_VERSION
                )
            })?;
            let owned =
                bayfat::owned_sectors(&mut view, &bays::set_dir(req.set), INFO_NAME, region)
                    .map_err(|e| format!("{e}: nothing was written"))?;
            for lba in owned {
                ed.allow_own(lba, SECTOR as u64);
            }
            let keep = |path: &str| add_pair.contains(&path);
            ed.add_nodes(&DirAt::Root, "", &nodes, &keep)?;
        }
    }
    let mut replaced = Vec::new();
    for (name, want) in replace {
        replaced.push(ed.replace_root_file(name, want)?);
    }

    // The slot, by the installer's write part: the payloads in place, read back by hash, then the
    // set's record. No pointer is moved: default.txt is the existing system's.
    let mut mem = Mem(std::mem::take(&mut ed.work));
    if mode == Mode::Added {
        // The sets just made: each record names the region its `reserve` holds.
        bays::write_empty_records(&mut mem).map_err(|e| format!("{e}: nothing was written"))?;
        bays::lay_out_empty_sets(&mut mem, &parts.layout)
            .map_err(|e| format!("{e}: nothing was written"))?;
    }
    let done = bays::stage_with(
        &mut mem,
        &parts.layout,
        req.set,
        req.slot,
        req.payloads,
        &Protected::default(),
    )
    .map_err(|e| format!("{e}: nothing was written"))?;
    if done != Done::Installed {
        return Err(format!(
            "the install found {} already in bay{}: nothing was written",
            req.slot, req.set
        ));
    }
    ed.work = mem.0;

    // The guard: every changed sector classified, before anything is given back.
    let changes = ed.audit()?;

    // The volume read again: sound, and every entry it had is where and what it was.
    let after = fatadd::survey(&ed.work)
        .map_err(|e| format!("the result does not read as a sound volume ({e})"))?;
    // The set the slot goes into is the install's to rewrite: its files and its `reserve` (its
    // directories and its record stay where they were).
    let own_prefix = format!("{}/", bays::set_dir(req.set)).to_ascii_uppercase();
    for old in &ed.survey.entries {
        let moved = replaced
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&old.path));
        let upper = old.path.to_ascii_uppercase();
        if upper.starts_with(&own_prefix)
            && !old.is_dir
            && !upper.ends_with(&format!("/{}", INFO_NAME.to_ascii_uppercase()))
        {
            continue;
        }
        let same = after.entries.iter().any(|new| new == old);
        if !moved && !same {
            return Err(format!(
                "{} is not where it was after the edit: the edit is refused and nothing was written",
                old.path
            ));
        }
    }
    let mut mem = Mem(std::mem::take(&mut ed.work));
    let placed = ed
        .added
        .iter()
        // A set's `reserve` is the install's to reshape: it holds what the files left of the region.
        .filter(|a| !a.is_dir && !a.path.ends_with(&format!("/{}", bayfat::RESERVE)))
        .map(|a| (a.path.clone(), a.size, a.first))
        .chain(replaced.iter().map(|r| (r.name.clone(), r.size, r.first)));
    for (path, size, first) in placed {
        let found = bootfat::locate_path(&mut mem, &path, size)
            .map_err(|e| format!("{path}: {e}: the edit is refused and nothing was written"))?;
        if found.first_lba != after.geo.cluster_lba(first) {
            return Err(format!(
                "{path} is not at the cluster it was given: the edit is refused and nothing was written"
            ));
        }
    }

    // The builder's own verification: exactly the expected lines are BAD.
    let findings = bootimage::verify(&mut mem, parts);
    let bad: Vec<&str> = findings
        .iter()
        .filter(|f| !f.ok)
        .map(|f| f.text.as_str())
        .collect();
    let exact = bad.len() == expected_bad.len()
        && expected_bad.iter().all(|name| {
            bad.iter()
                .filter(|line| line.starts_with(&format!("{name}: ")))
                .count()
                == 1
        });
    if !exact {
        return Err(format!(
            "the result's verification is not the one expected: the BAD lines are [{}] and exactly one each is expected for [{}]: nothing was written",
            bad.join("; "),
            expected_bad.join(", ")
        ));
    }
    let image = mem.0;
    let undo = fatadd::undo_file(dump, &changes);
    Ok(Outcome {
        image,
        mode,
        changes,
        undo,
        added: ed.added,
        replaced,
        left,
        findings,
        expected_bad,
        free_before,
        free_after: after.free_map(),
    })
}

/// The path of a set's record, for a caller that reads it back.
pub fn info_path(set: u8) -> String {
    format!("{}/{INFO_NAME}", bays::set_dir(set))
}

#[cfg(test)]
#[path = "bootadd_tests.rs"]
mod tests;
