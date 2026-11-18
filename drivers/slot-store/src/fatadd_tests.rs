//! Tests of adding to an existing FAT16 volume: the free map and the allocation rule, each refusal,
//! the audit that guards what already exists, and the undo file. The volumes are small (8 MiB, one
//! sector per cluster) unless a test says otherwise.

use super::*;
use crate::bootimage::Mem;
use crate::fatimage::{seeded, tree};

/// A file node with seeded bytes.
fn file(name: &'static str, seed: u64, len: usize) -> Node {
    Node::File {
        name,
        data: seeded(seed, len),
    }
}

/// A name that lives as long as the test binary (the tree's names are `'static`).
fn leak(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// Removes the root file `name` the way a FAT driver does: its entry marked deleted, its chain
/// freed in every FAT copy. Its bytes stay.
fn delete(image: &mut [u8], name: &str) {
    let sv = survey(image).unwrap();
    let e = sv
        .find(name)
        .unwrap_or_else(|| panic!("{name} is there"))
        .clone();
    image[e.at] = 0xE5;
    for c in e.chain {
        for copy in 0..sv.geo.fats {
            let at = sv.geo.fat_at(copy, c);
            image[at..at + 2].copy_from_slice(&[0, 0]);
        }
    }
}

/// A volume whose free clusters are three runs: 100, then 120, then `tail` at the end.
fn fragmented(tail: usize) -> Vec<u8> {
    let clusters = Geometry::parse(&tree(&[])).unwrap().clusters as usize;
    let fill = clusters - (10 + 100 + 10 + 120 + 10) - tail;
    let mut image = tree(&[
        file("keep0.bin", 1, 10 * 512),
        file("hole0.bin", 2, 100 * 512),
        file("keep1.bin", 3, 10 * 512),
        file("hole1.bin", 4, 120 * 512),
        file("keep2.bin", 5, 10 * 512),
        file("fill.bin", 6, fill * 512),
    ]);
    delete(&mut image, "hole0.bin");
    delete(&mut image, "hole1.bin");
    image
}

/// Every existing entry's bytes, read through the reader.
fn contents(image: &[u8]) -> Vec<(Existing, Vec<u8>)> {
    let sv = survey(image).unwrap();
    let mut mem = Mem(image.to_vec());
    sv.entries
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| {
            let bytes = bootfat::read_file(&mut mem, &e.path, u64::MAX).unwrap();
            (e.clone(), bytes)
        })
        .collect()
}

#[test]
fn first_fit_gives_the_lowest_run_that_holds_the_file_and_none_when_no_run_does() {
    // Clusters 2..5 free, 6 used, 7..9 free, 10 used, 11..20 free.
    let mut free = vec![false; 21];
    for c in (2..6).chain(7..10).chain(11..21) {
        free[c] = true;
    }
    assert_eq!(first_fit(&free, 1), Some(2));
    assert_eq!(first_fit(&free, 4), Some(2));
    assert_eq!(
        first_fit(&free, 5),
        Some(11),
        "the first two runs are too short: never split"
    );
    assert_eq!(first_fit(&free, 10), Some(11));
    assert_eq!(first_fit(&free, 11), None);
    assert_eq!(
        free_map(&free),
        FreeMap {
            clusters: 17,
            runs: 3,
            longest: 10
        }
    );
}

#[test]
fn a_cluster_is_free_only_when_its_entry_is_zero_and_the_survey_lists_every_chain() {
    let image = tree(&[
        file("a.bin", 1, 3 * 512),
        Node::Dir {
            name: "dir",
            children: vec![file("a long name.bin", 2, 700)],
        },
    ]);
    let sv = survey(&image).unwrap();
    let paths: Vec<&str> = sv.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["A.BIN", "DIR", "DIR/a long name.bin"]);
    assert_eq!(sv.entries[0].chain, [2, 3, 4]);
    assert_eq!(sv.entries[1].chain, [5]);
    assert_eq!(sv.entries[2].chain, [6, 7]);
    assert!(!sv.free[7] && sv.free[8]);
    assert_eq!(sv.free_map().clusters, sv.geo.clusters - 6);
    assert_eq!(sv.owner_of(6).unwrap().path, "DIR/a long name.bin");
}

#[test]
fn a_fragmented_free_map_with_no_run_for_a_file_is_refused_by_name_and_nothing_changes() {
    let image = fragmented(0);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    // 100 clusters fit the first hole; 121 fit neither, though 220 are free.
    ed.add_file(&DirAt::Root, "", "small.bin", &seeded(7, 100 * 512))
        .unwrap();
    let err = ed
        .add_file(&DirAt::Root, "", "big.bin", &seeded(8, 121 * 512))
        .unwrap_err();
    assert_eq!(
        err,
        "big.bin: it needs one run of 121 free clusters (61952 bytes) and the longest free run is 120 clusters (120 free in 1 runs): a file is never split: nothing was written"
    );
}

#[test]
fn a_run_that_exists_only_later_in_the_volume_is_found_and_the_file_is_not_split() {
    let image = fragmented(300);
    let before = contents(&image);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let last = ed.survey.geo.clusters + 2;
    ed.add_file(&DirAt::Root, "", "big.bin", &seeded(8, 121 * 512))
        .unwrap();
    ed.add_file(&DirAt::Root, "", "small.bin", &seeded(7, 512))
        .unwrap();
    // The big file took the run at the end, whole; the small one the first hole.
    assert_eq!((ed.added[0].first, ed.added[0].clusters), (last - 300, 121));
    assert_eq!((ed.added[1].first, ed.added[1].clusters), (12, 1));
    ed.audit().unwrap();
    let mut mem = Mem(ed.work.clone());
    assert_eq!(
        bootfat::locate_path(&mut mem, "big.bin", 121 * 512)
            .unwrap()
            .first_lba,
        ed.survey.geo.cluster_lba(last - 300)
    );
    assert_eq!(
        bootfat::read_file(&mut mem, "big.bin", u64::MAX).unwrap(),
        seeded(8, 121 * 512)
    );
    assert_eq!(
        contents(&ed.work)[..before.len()],
        before[..],
        "every old file is where and what it was"
    );
}

#[test]
fn a_full_volume_is_refused_by_name() {
    let clusters = Geometry::parse(&tree(&[])).unwrap().clusters as usize;
    let image = tree(&[file("fill.bin", 1, clusters * 512)]);
    assert_eq!(survey(&image).unwrap().free_map().clusters, 0);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let err = ed.add_file(&DirAt::Root, "", "one.bin", b"x").unwrap_err();
    assert_eq!(
        err,
        "one.bin: it needs one run of 1 free clusters (512 bytes) and the longest free run is 0 clusters (0 free in 0 runs): a file is never split: nothing was written"
    );
}

#[test]
fn a_full_root_directory_is_refused() {
    let nodes: Vec<Node> = (0..512)
        .map(|i| file(leak(format!("f{i:03}.bin")), i, 10))
        .collect();
    let image = tree(&nodes);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let err = ed.add_file(&DirAt::Root, "", "one.bin", b"x").unwrap_err();
    assert_eq!(
        err,
        "the root directory is full: it holds 512 entries, 512 are used and 1 more are needed: nothing was written"
    );
    // A long name needs its long-name entries too: 510 used leaves room for two, not for three.
    let image = tree(&nodes[..510]);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let err = ed
        .add_file(&DirAt::Root, "", "a name of twenty chars", b"x")
        .unwrap_err();
    assert!(
        err.starts_with(
            "the root directory is full: it holds 512 entries, 510 are used and 3 more"
        ),
        "{err}"
    );
    ed.add_file(&DirAt::Root, "", "one.bin", b"x").unwrap();
}

#[test]
fn a_name_that_already_exists_is_refused_by_name_whether_long_or_short() {
    let image = tree(&[
        file("reason.rec", 1, 10),
        file("a long name.bin", 2, 10),
        Node::Dir {
            name: "slots",
            children: vec![file("inner.bin", 3, 10)],
        },
    ]);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    for name in [
        "reason.rec",
        "REASON.REC",
        "A Long Name.bin",
        "ALONGN~1.BIN",
        "slots",
    ] {
        let err = ed.add_file(&DirAt::Root, "", name, b"x").unwrap_err();
        assert_eq!(
            err,
            format!("{name} is already on the volume: nothing was written")
        );
    }
    // A directory wanted where a file is.
    let wanted = [Node::Dir {
        name: "reason.rec",
        children: vec![file("x.bin", 4, 1)],
    }];
    let err = ed
        .add_nodes(&DirAt::Root, "", &wanted, &|_| true)
        .unwrap_err();
    assert_eq!(
        err,
        "reason.rec is a file on the volume and a directory is needed there: nothing was written"
    );
    assert!(ed.work == image, "nothing changed");
}

#[test]
fn fat_copies_that_differ_are_refused() {
    let mut image = tree(&[file("a.bin", 1, 3 * 512)]);
    let geo = Geometry::parse(&image).unwrap();
    // Cluster 9 is free in the first copy and marked used in the second.
    let at = geo.fat_at(1, 9);
    image[at..at + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
    let err = survey(&image).unwrap_err();
    assert_eq!(
        err,
        "the FAT copies differ (copy 1 and copy 2 at the entry of cluster 9): the volume is not sound: nothing was written"
    );
    assert!(Editor::new(&image, &first_fit).is_err());
}

#[test]
fn fat12_fat32_and_a_file_that_is_not_fat_are_refused_by_name() {
    // FAT12: the same volume read with 8 sectors per cluster has 2,027 clusters.
    let mut fat12 = tree(&[]);
    fat12[13] = 8;
    let err = survey(&fat12).unwrap_err();
    assert_eq!(
        err,
        "a FAT12 volume (2027 clusters): --add-to writes FAT16 only: nothing was written"
    );
    // FAT32 as its boot sector says it: no 16-bit FAT size, no fixed root directory.
    let mut fat32 = tree(&[]);
    fat32[17..19].copy_from_slice(&[0, 0]);
    fat32[22..24].copy_from_slice(&[0, 0]);
    let err = survey(&fat32).unwrap_err();
    assert!(
        err.starts_with("a FAT32 volume (no 16-bit FAT size or no fixed root directory)"),
        "{err}"
    );
    // FAT32 by its cluster count.
    let mut many = tree(&[]);
    many[32..36].copy_from_slice(&200_000u32.to_le_bytes());
    let err = survey(&many).unwrap_err();
    assert!(err.starts_with("a FAT32 volume (199"), "{err}");
    // Not FAT at all: no boot signature; not whole sectors; an empty file.
    let err = survey(&seeded(1, 8 * 1024 * 1024)).unwrap_err();
    assert_eq!(
        err,
        "not a FAT16 volume: the first sector has no boot signature (55 AA): nothing was written"
    );
    assert!(
        survey(&[0u8; 700])
            .unwrap_err()
            .starts_with("not a FAT16 volume: 700 bytes is not a whole")
    );
    assert!(survey(&[]).is_err());
    // A dump cut short of the volume its boot sector describes.
    let whole = tree(&[]);
    let err = survey(&whole[..whole.len() - 512]).unwrap_err();
    assert!(err.contains("not a dump of the whole partition"), "{err}");
}

#[test]
fn a_volume_that_is_not_sound_is_refused_before_anything_is_added() {
    let image = tree(&[file("a.bin", 1, 3 * 512), file("b.bin", 2, 3 * 512)]);
    let geo = Geometry::parse(&image).unwrap();
    let set = |image: &mut Vec<u8>, c: u64, v: u16| {
        for copy in 0..geo.fats {
            let at = geo.fat_at(copy, c);
            image[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
    };
    // b.bin's chain runs into a.bin's: one cluster in two chains.
    let mut cross = image.clone();
    set(&mut cross, 5, 3);
    let err = survey(&cross).unwrap_err();
    assert!(
        err.starts_with("cluster 3 is in the chains of both A.BIN and B.BIN"),
        "{err}"
    );
    // A chain through a free cluster.
    let mut hole = image.clone();
    set(&mut hole, 3, 0);
    assert!(
        survey(&hole)
            .unwrap_err()
            .contains("which the FAT marks free or bad")
    );
    // A chain shorter than the size.
    let mut short = image.clone();
    set(&mut short, 2, 0xFFFF);
    assert!(
        survey(&short)
            .unwrap_err()
            .contains("its chain is 1 clusters and its size of 1536 bytes needs 3")
    );
    // Bytes after the root directory's end marker.
    let mut stale = image.clone();
    let root = geo.root_start as usize * 512;
    stale[root + 5 * 32 + 1] = 7;
    assert!(
        survey(&stale)
            .unwrap_err()
            .starts_with("the root directory has bytes after its end marker")
    );
}

#[test]
fn an_allocator_that_hands_out_a_used_cluster_is_refused_by_the_audit() {
    let image = tree(&[file("old.bin", 1, 4 * 512), file("other.bin", 2, 512)]);
    // The mutation: the allocator ignores the map and gives cluster 3, inside old.bin.
    let broken = |_: &[bool], _: u64| Some(3u64);
    let mut ed = Editor::new(&image, &broken).unwrap();
    ed.add_file(&DirAt::Root, "", "new.bin", &seeded(9, 512))
        .unwrap();
    assert!(ed.work != image, "the broken edit did change the copy");
    let err = ed.audit().unwrap_err();
    assert!(
        err.contains("would touch an existing file's cluster: cluster 3 belongs to OLD.BIN")
            && err.ends_with("the edit is refused and nothing was written"),
        "{err}"
    );
    // The same allocator with the bytes the cluster already holds: no data sector differs, and the
    // audit still refuses, by what the allocation was given.
    let mut ed = Editor::new(&image, &broken).unwrap();
    let same = seeded(1, 4 * 512)[512..1024].to_vec();
    ed.add_file(&DirAt::Root, "", "new.bin", &same).unwrap();
    let err = ed.audit().unwrap_err();
    assert!(
        err.contains("a new file was given cluster 3 and would touch an existing file's cluster"),
        "{err}"
    );
    // The same edit with the real rule passes: the refusal is the allocator's doing.
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.add_file(&DirAt::Root, "", "new.bin", &seeded(9, 512))
        .unwrap();
    ed.audit().unwrap();
}

#[test]
fn the_audit_refuses_every_change_to_what_was_there() {
    let image = tree(&[
        file("old.bin", 1, 4 * 512),
        Node::Dir {
            name: "dir",
            children: vec![file("in.bin", 2, 512)],
        },
    ]);
    let geo = Geometry::parse(&image).unwrap();
    let tampered = |at: usize| {
        let mut ed = Editor::new(&image, &first_fit).unwrap();
        ed.add_file(&DirAt::Root, "", "new.bin", b"new").unwrap();
        ed.work[at] ^= 0xFF;
        ed.audit().unwrap_err()
    };
    // A byte of an existing file.
    let err = tampered(geo.cluster_lba(4) as usize * 512 + 9);
    assert!(
        err.contains("would touch an existing file's cluster: cluster 4 belongs to OLD.BIN"),
        "{err}"
    );
    // The boot sector.
    assert!(tampered(3).contains("sector 0 is the boot sector or a reserved sector"));
    // An existing root entry, and an existing entry of a subdirectory.
    let root = geo.root_start as usize * 512;
    assert!(
        tampered(root + 28)
            .contains("an existing directory entry of the root directory (sector 129, entry 0)")
    );
    let dir = geo.cluster_lba(6) as usize * 512;
    assert!(tampered(dir + 2 * 32).contains("an existing directory entry of DIR"));
    // An existing FAT entry, in either copy.
    let err = tampered(geo.fat_at(0, 2));
    assert!(err.contains("FAT copy 2 would differ from copy 1"), "{err}");
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.set_fat(2, 0xFFFF);
    assert!(
        ed.audit()
            .unwrap_err()
            .contains("the FAT entry of cluster 2 (OLD.BIN) would change from 0x0003 to 0xffff")
    );
    // A free cluster no new file was given.
    let err = tampered(geo.cluster_lba(500) as usize * 512);
    assert!(
        err.contains("is in the free cluster 500, which no new file was given"),
        "{err}"
    );
    // And with nothing tampered the same edit passes.
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.add_file(&DirAt::Root, "", "new.bin", b"new").unwrap();
    ed.audit().unwrap();
}

#[test]
fn files_go_into_new_and_existing_directories_and_the_reader_finds_each_one_whole() {
    let image = tree(&[
        file("old.bin", 1, 4 * 512),
        Node::Dir {
            name: "slots",
            children: vec![Node::Dir {
                name: "an older system's slot",
                children: vec![file("kernel8.img", 2, 5000)],
            }],
        },
    ]);
    let before = contents(&image);
    let wanted = [
        file("slot-layout.toml", 10, 900),
        file("trial.rec", 11, 1024),
        Node::Dir {
            name: "slots",
            children: vec![Node::Dir {
                name: "bay0",
                children: vec![
                    file("kernel8.img", 12, 9000),
                    Node::Dir {
                        name: "overlays",
                        children: vec![
                            file("disable-bt-pi5.dtbo", 13, 100),
                            file("disable-wifi-pi5.dtbo", 14, 100),
                            file("README", 15, 40),
                        ],
                    },
                ],
            }],
        },
    ];
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.add_nodes(&DirAt::Root, "", &wanted, &|_| true).unwrap();
    let changes = ed.audit().unwrap();
    let classes: Vec<Class> = changes.iter().map(|c| c.class).collect();
    for class in [
        Class::Fat(1),
        Class::Fat(2),
        Class::RootDir,
        Class::Dir,
        Class::NewDir,
        Class::NewFile,
    ] {
        assert!(classes.contains(&class), "{class} is among {classes:?}");
    }
    let listed: Vec<(&str, bool)> = ed
        .added
        .iter()
        .map(|a| (a.path.as_str(), a.is_dir))
        .collect();
    assert_eq!(
        listed,
        [
            ("slot-layout.toml", false),
            ("trial.rec", false),
            ("slots/bay0", true),
            ("slots/bay0/kernel8.img", false),
            ("slots/bay0/overlays", true),
            ("slots/bay0/overlays/disable-bt-pi5.dtbo", false),
            ("slots/bay0/overlays/disable-wifi-pi5.dtbo", false),
            ("slots/bay0/overlays/README", false),
        ]
    );
    let mut mem = Mem(ed.work.clone());
    for (path, seed, len) in [
        ("slot-layout.toml", 10, 900),
        ("trial.rec", 11, 1024),
        ("slots/bay0/kernel8.img", 12, 9000),
        ("slots/bay0/overlays/disable-bt-pi5.dtbo", 13, 100),
        ("slots/bay0/overlays/disable-wifi-pi5.dtbo", 14, 100),
        ("slots/bay0/overlays/README", 15, 40),
    ] {
        bootfat::locate_path(&mut mem, path, len as u64).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(
            bootfat::read_file(&mut mem, path, u64::MAX).unwrap(),
            seeded(seed, len),
            "{path}"
        );
    }
    // The record's 8.3 name is the one the raw locator looks up.
    bootfat::locate(&mut mem, b"TRIAL   REC", 1024).unwrap();
    // Two long names with the same first six letters get different short names.
    let after = survey(&ed.work).unwrap();
    let dir = after.find("slots/bay0/overlays").unwrap().chain.clone();
    let shorts: Vec<[u8; 11]> = listed_of(&ed.work, &after.geo, &dir);
    assert!(
        shorts.contains(b"DISABL~1DTB") && shorts.contains(b"DISABL~2DTB"),
        "{shorts:?}"
    );
    // The new directory's `..` names its parent, the existing `slots`.
    let slots = after.find("slots").unwrap().chain[0];
    let bay = after.find("slots/bay0").unwrap().chain[0];
    let at = after.geo.cluster_lba(bay) as usize * 512;
    assert_eq!(&ed.work[at..at + 11], b".          ");
    assert_eq!(
        u16::from_le_bytes([ed.work[at + 26], ed.work[at + 27]]) as u64,
        bay
    );
    assert_eq!(
        u16::from_le_bytes([ed.work[at + 58], ed.work[at + 59]]) as u64,
        slots
    );
    assert_eq!(contents(&ed.work)[..1], before[..1]);
    assert!(
        after
            .find("slots/an older system's slot/kernel8.img")
            .is_some()
    );
}

/// The short names of a directory's entries.
fn listed_of(image: &[u8], geo: &Geometry, chain: &[u64]) -> Vec<[u8; 11]> {
    listed(&dir_bytes(image, &slots_of(geo, chain)))
        .iter()
        .map(|e| e.raw)
        .collect()
}

#[test]
fn a_full_subdirectory_is_refused_by_name() {
    // One sector per cluster: a directory cluster holds 16 entries, two of them `.` and `..`.
    let children: Vec<Node> = (0..14)
        .map(|i| file(leak(format!("f{i}.bin")), i, 1))
        .collect();
    let image = tree(&[Node::Dir {
        name: "slots",
        children,
    }]);
    let wanted = [Node::Dir {
        name: "slots",
        children: vec![file("one.bin", 1, 1)],
    }];
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let err = ed
        .add_nodes(&DirAt::Root, "", &wanted, &|_| true)
        .unwrap_err();
    assert_eq!(
        err,
        "the directory slots is full: it holds 16 entries, 16 are used and 1 more are needed: nothing was written"
    );
}

#[test]
fn a_replaced_root_file_keeps_its_entry_and_its_old_clusters_are_freed_not_reused() {
    let image = tree(&[
        file("old.bin", 1, 2 * 512),
        file("trial.txt", 2, 55),
        file("after.bin", 3, 512),
    ]);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    let old = ed.survey.find("trial.txt").unwrap().clone();
    let new = seeded(4, 1024);
    let r = ed.replace_root_file("trial.txt", &new).unwrap();
    ed.add_file(&DirAt::Root, "", "more.bin", b"x").unwrap();
    assert_eq!((r.old_size, r.old_first, r.old_clusters), (55, 4, 1));
    assert_eq!(
        r.old_sha256,
        <[u8; 32]>::from(Sha256::digest(seeded(2, 55)))
    );
    assert_eq!((r.size, r.first, r.clusters), (1024, 6, 2));
    assert_eq!(
        ed.added[0].first, 8,
        "the freed cluster 4 is not handed out in the same edit"
    );
    let changes = ed.audit().unwrap();
    let after = survey(&ed.work).unwrap();
    let now = after.find("trial.txt").unwrap();
    assert_eq!(now.at, old.at, "the same directory entry");
    assert_eq!(
        (now.first, now.size, now.chain.as_slice()),
        (6, 1024, &[6u64, 7][..])
    );
    assert!(after.free[4], "the old cluster is free in every copy");
    // The old bytes are still in the freed cluster: nothing wrote there.
    let at = after.geo.cluster_lba(4) as usize * 512;
    assert_eq!(ed.work[at..at + 55], seeded(2, 55)[..]);
    // The undo gives the original back.
    let undo = undo_file(&image, &changes);
    assert!(apply_undo(&ed.work, &undo).unwrap() == image);
    // A file that is not there is refused.
    let err = ed.replace_root_file("absent.txt", b"x").unwrap_err();
    assert_eq!(
        err,
        "absent.txt is not a file in the root directory: nothing was written"
    );
}

#[test]
fn the_changed_list_is_exact_and_the_undo_file_gives_the_original_back() {
    let image = fragmented(300);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.add_file(&DirAt::Root, "", "big.bin", &seeded(8, 121 * 512))
        .unwrap();
    ed.add_file(&DirAt::Root, "", "a long name.txt", &seeded(9, 700))
        .unwrap();
    let changes = ed.audit().unwrap();
    // Sorted, not overlapping, and every range's hashes are of its bytes.
    for pair in changes.windows(2) {
        assert!(pair[0].lba + pair[0].count <= pair[1].lba);
    }
    // Only the listed sectors, copied onto the original, give the edited volume.
    let mut applied = image.clone();
    for c in &changes {
        let (from, to) = (c.lba as usize * 512, (c.lba + c.count) as usize * 512);
        assert_eq!(c.old, <[u8; 32]>::from(Sha256::digest(&image[from..to])));
        assert_eq!(c.new, <[u8; 32]>::from(Sha256::digest(&ed.work[from..to])));
        applied[from..to].copy_from_slice(&ed.work[from..to]);
    }
    assert!(applied == ed.work, "the list is complete");
    // Every listed sector does differ: the list is exact.
    for c in &changes {
        for lba in c.lba..c.lba + c.count {
            let (from, to) = (lba as usize * 512, (lba + 1) as usize * 512);
            assert!(
                image[from..to] != ed.work[from..to],
                "sector {lba} is listed and unchanged"
            );
        }
    }
    let undo = undo_file(&image, &changes);
    assert!(apply_undo(&ed.work, &undo).unwrap() == image);
    let ranges: Vec<(u64, u64)> = changes.iter().map(|c| (c.lba, c.count)).collect();
    assert_eq!(undo_ranges(&undo).unwrap(), ranges);
    // The same edit again gives the same list and the same undo file.
    let mut again = Editor::new(&image, &first_fit).unwrap();
    again
        .add_file(&DirAt::Root, "", "big.bin", &seeded(8, 121 * 512))
        .unwrap();
    again
        .add_file(&DirAt::Root, "", "a long name.txt", &seeded(9, 700))
        .unwrap();
    assert_eq!(again.audit().unwrap(), changes);
    assert!(again.work == ed.work);
}

#[test]
fn an_undo_file_is_applied_only_to_the_volume_its_edit_left() {
    let image = tree(&[file("old.bin", 1, 2 * 512)]);
    let mut ed = Editor::new(&image, &first_fit).unwrap();
    ed.add_file(&DirAt::Root, "", "new.bin", &seeded(2, 600))
        .unwrap();
    let undo = undo_file(&image, &ed.audit().unwrap());
    // The original itself, and the edited volume written since: both refused, nothing given back.
    let err = apply_undo(&image, &undo).unwrap_err();
    assert!(
        err.contains("do not hold what the edit left there"),
        "{err}"
    );
    let mut later = ed.work.clone();
    let at = ed.survey.geo.cluster_lba(ed.added[0].first) as usize * 512;
    later[at] ^= 1;
    assert!(apply_undo(&later, &undo).is_err());
    // Another size, a cut file, and a file that is no undo file.
    assert!(
        apply_undo(&ed.work[..ed.work.len() - 512], &undo)
            .unwrap_err()
            .contains("is for a volume of 16384 sectors")
    );
    assert!(
        apply_undo(&ed.work, &undo[..undo.len() - 1])
            .unwrap_err()
            .starts_with("not an undo file")
    );
    assert!(
        apply_undo(&ed.work, b"something else")
            .unwrap_err()
            .starts_with("not an undo file")
    );
}
