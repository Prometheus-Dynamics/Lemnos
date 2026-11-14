//! The driver's operations under a power cut at every sector write, each as its own sweep. The
//! operations are the calls the system-slots driver makes on the boot partition's raw volume: an
//! install (`install_slot`), a trial arm (`arm_trial`), a confirmation (`set_pointer(Default)` then
//! `clear_trial`, the order `resolve.rs` uses) and a removal (`free_slot`); the U-Boot form's install
//! and removal go through `BayVolume` with the environment's protected bays (its default, previous
//! and trial are environment variables: one two-copy environment write each, swept at byte level in
//! `uboot_env_tests` and in slot-init's fallback test).
//!
//! Every write of the operation is a cut point and each cut is torn four ways (the sector all old,
//! all new, first half new, random bytes). After every cut: the boot chain's reading (the default
//! pointer's bay, its record and every stored file against the slot's manifest, here the payload
//! bytes) finds a bootable default; `bootimage::verify` (the code behind `boot-partition --verify`)
//! accepts the image; no sector outside the operation's write set was written or changed, so the FAT
//! (boot sector, both FATs, every directory cluster) is byte-identical; and the operation, run again
//! on the cut image, completes.

use std::collections::HashSet;

use super::*;
use crate::boot_raw::BayVolume;
use crate::bootimage::{self, Family, Fixed, Parts};

/// The payloads of slot `d`.
fn payloads_d() -> [Vec<u8>; 5] {
    [
        vec![21; 1800],
        vec![22; 200],
        vec![23; 100],
        b"console=tty0 root=lemnos-d".to_vec(),
        vec![24; 50],
    ]
}

/// A slot's payloads by name: what its manifest lists.
fn manifest(slot: &str) -> [Vec<u8>; 5] {
    match slot {
        "a" => payloads_a(),
        "b" => super::payloads_b(),
        "c" => payloads_c(),
        "d" => payloads_d(),
        other => panic!("no slot {other}"),
    }
}

fn pi_image() -> Vec<u8> {
    bootimage::build(
        &super::boot_parts(&test_layout()),
        BOARD_SECTORS * 512,
        "a",
        &refs(&payloads_a()),
    )
    .expect("the image builds")
}

fn pi_parts() -> Parts {
    super::boot_parts(&test_layout())
}

fn uboot_parts() -> Parts {
    Parts {
        family: Family::UbootBlock,
        layout: test_layout(),
        root: vec![("slot-layout.toml", b"version = 5\n".to_vec())],
        fixed: vec![Fixed {
            path: bootimage::EXTLINUX,
            bytes: Vec::new(),
            size: bootimage::EXTLINUX_BYTES,
        }],
    }
}

fn uboot_image() -> Vec<u8> {
    bootimage::build(
        &uboot_parts(),
        BOARD_SECTORS * 512,
        "a",
        &refs(&payloads_a()),
    )
    .expect("the image builds")
}

/// The device's volume run through one driver operation, uncut.
type Run<'a> = &'a dyn Fn(Dev) -> (Dev, Result<(), String>);

fn pi(img: Vec<u8>, f: impl FnOnce(&mut Boot<Dev>) -> Result<(), String>) -> Vec<u8> {
    let mut boot = Boot::on(Dev::new(img)).unwrap();
    f(&mut boot).unwrap();
    boot.into_device().img
}

fn install_pi(img: Vec<u8>, slot: &str) -> Vec<u8> {
    pi(img, |b| {
        b.install_slot(slot, &refs(&manifest(slot))).map(|_| ())
    })
}

/// The history the sweeps start from: `a` default; `b` installed and made default (`a` previous);
/// `c` installed; a trial of `c` armed; `c` confirmed (`b` previous, `a` unprotected); `a` removed.
struct History {
    /// `a` default, `b` and `c` not installed.
    h0: Vec<u8>,
    /// `b` default, `a` previous.
    h2: Vec<u8>,
    /// `c` installed into the one free bay.
    h3: Vec<u8>,
    /// A trial of `c` armed.
    h4: Vec<u8>,
    /// `c` confirmed: default `c`, previous `b`, `a`'s bay holds `a`'s record, named by nothing.
    h5: Vec<u8>,
}

fn history() -> History {
    let h0 = pi_image();
    let h1 = install_pi(h0.clone(), "b");
    let h2 = pi(h1, |b| b.set_pointer(Pointer::Default, "b"));
    let h3 = install_pi(h2.clone(), "c");
    let h4 = pi(h3.clone(), |b| b.arm_trial("c", 3));
    let h5 = pi(h4.clone(), |b| {
        b.set_pointer(Pointer::Default, "c")?;
        b.clear_trial()
    });
    History { h0, h2, h3, h4, h5 }
}

/// The sectors of a set's payload files and record pair.
fn set_sectors(placed: &Placed, layout: &Layout, set: u8, out: &mut HashSet<u64>) {
    for (i, lba) in placed.files[set as usize].iter().enumerate() {
        for k in 0..layout.bytes[i] / SECTOR as u64 {
            out.insert(lba + k);
        }
    }
    out.insert(placed.info[set as usize]);
    out.insert(placed.info[set as usize] + 1);
}

fn pair(lba: u64, out: &mut HashSet<u64>) {
    out.insert(lba);
    out.insert(lba + 1);
}

/// A set's stored files are exactly the slot's payloads (what the slot's manifest lists), each at its
/// stored length.
fn files_are(dev: &mut Dev, placed: &Placed, set: u8, slot: &str) -> Result<(), String> {
    let info = read_info(dev, placed.info[set as usize])
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("bay{set} has no record"))?;
    if info.slot != slot {
        return Err(format!("bay{set} holds {}, not {slot}", info.slot));
    }
    let want = manifest(slot);
    for i in 0..5 {
        let (len, _) = info.files[i];
        if len != want[i].len() as u64 {
            return Err(format!(
                "bay{set} ({slot}): {} is {len} bytes, the manifest's is {}",
                ROLES[i].role,
                want[i].len()
            ));
        }
        let got = read_file(dev, placed.files[set as usize][i], len).map_err(|e| e.to_string())?;
        if got != want[i] {
            return Err(format!(
                "bay{set} ({slot}): {} differs from the manifest's",
                ROLES[i].role
            ));
        }
    }
    Ok(())
}

/// What the sweep expects after every cut.
struct Expect<'a> {
    /// The slots the default pointer may name (bay, slot): before or after the operation.
    defaults: &'a [(u8, &'a str)],
    /// The slots the previous pointer may name.
    previous: &'a [(u8, &'a str)],
}

/// The boot chain's reading of a cut Pi image, and the verifier's.
fn pi_chain(img: &[u8], layout: &Layout, e: &Expect, parts: &Parts) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    let placed = place(&mut dev, layout)?;
    let d = boot_view(&mut dev, layout, placed.default)?;
    if !e.defaults.iter().any(|(s, n)| (*s, n.to_string()) == d) {
        return Err(format!("the default boots {d:?}, allowed {:?}", e.defaults));
    }
    files_are(&mut dev, &placed, d.0, &d.1)?;
    let p = boot_view(&mut dev, layout, placed.previous)?;
    if !e.previous.iter().any(|(s, n)| (*s, n.to_string()) == p) {
        return Err(format!(
            "the previous boots {p:?}, allowed {:?}",
            e.previous
        ));
    }
    files_are(&mut dev, &placed, p.0, &p.1)?;
    // The protected sets as the next install reads them name the default and the previous.
    let prot = Protected::from_volume(&mut dev, layout).map_err(|e| e.to_string())?;
    if prot.default != Some(d.0) || prot.previous != Some(p.0) {
        return Err(format!("the protected sets are {prot:?}"));
    }
    // A pending trial record names a slot whose bay the trial pointer names and whose files are whole.
    if let Some(t) = pending_trial(&mut dev, placed.trial).map_err(|e| e.to_string())? {
        let set = read_pointer(&mut dev, placed.trial_pointer)
            .map_err(|e| e.to_string())?
            .ok_or("a trial record with no trial pointer")?;
        files_are(&mut dev, &placed, set, &t.slot).map_err(|e| format!("the trial: {e}"))?;
    }
    // No set holds a record that its files do not match, in any cut state.
    for set in 0..SETS {
        if let Some(info) =
            read_info(&mut dev, placed.info[set as usize]).map_err(|e| e.to_string())?
        {
            files_are(&mut dev, &placed, set, &info.slot)?;
        }
    }
    let bad: Vec<String> = bootimage::verify(&mut bootimage::Mem(img.to_vec()), parts)
        .into_iter()
        .filter(|f| !f.ok)
        .map(|f| f.text)
        .collect();
    if !bad.is_empty() {
        return Err(format!("boot-partition --verify: {bad:?}"));
    }
    Ok(())
}

/// Every sector written lies inside `allowed`, and every sector outside it is byte-identical to `pre`
/// (the FAT's boot sector, both FATs and all directory clusters are outside it).
fn only_inside(pre: &[u8], dev: &Dev, allowed: &HashSet<u64>) -> Result<(), String> {
    if let Some(bad) = dev.written.iter().find(|l| !allowed.contains(l)) {
        return Err(format!(
            "wrote sector {bad}, outside the operation's write set"
        ));
    }
    let mut start = 0u64;
    for lba in 0..=BOARD_SECTORS {
        if lba == BOARD_SECTORS || allowed.contains(&lba) {
            if lba > start {
                let (a, b) = (start as usize * SECTOR, lba as usize * SECTOR);
                if pre[a..b] != dev.img[a..b] {
                    let at = (a..b).find(|i| pre[*i] != dev.img[*i]).unwrap() / SECTOR;
                    return Err(format!("sector {at} changed outside the write set"));
                }
            }
            start = lba + 1;
        }
    }
    Ok(())
}

struct Totals {
    writes: usize,
    runs: usize,
}

/// The sweep. `chain` reads a cut image as the boot chain does; `done` checks the image after the
/// operation completed (uncut, or re-run from a cut).
fn sweep(
    name: &str,
    pre: &[u8],
    run: Run,
    allowed: &HashSet<u64>,
    chain: &dyn Fn(&[u8]) -> Result<(), String>,
    done: &dyn Fn(&[u8]) -> Result<(), String>,
) -> Totals {
    let (uncut, result) = run(Dev::new(pre.to_vec()));
    result.unwrap_or_else(|e| panic!("{name}: the uncut operation failed: {e}"));
    let writes = uncut.writes;
    assert!(writes > 0, "{name}: the operation writes");
    only_inside(pre, &uncut, allowed).unwrap_or_else(|e| panic!("{name}: uncut: {e}"));
    done(&uncut.img).unwrap_or_else(|e| panic!("{name}: uncut: {e}"));
    let mut runs = 0;
    for cut in 0..writes {
        for torn in [
            Torn::Old,
            Torn::New,
            Torn::FirstHalfNew,
            Torn::Random(cut as u64 + 1),
        ] {
            let mut dev = Dev::new(pre.to_vec());
            dev.cut = Some((cut, torn));
            let (after, result) = run(dev);
            assert!(
                result.is_err(),
                "{name}: cut {cut} {torn:?}: the cut write must fail the operation"
            );
            chain(&after.img).unwrap_or_else(|e| panic!("{name}: cut {cut} {torn:?}: {e}"));
            only_inside(pre, &after, allowed)
                .unwrap_or_else(|e| panic!("{name}: cut {cut} {torn:?}: {e}"));
            let (again, result) = run(Dev::new(after.img.clone()));
            result.unwrap_or_else(|e| panic!("{name}: cut {cut} {torn:?}: the re-run failed: {e}"));
            done(&again.img)
                .unwrap_or_else(|e| panic!("{name}: cut {cut} {torn:?}: after the re-run: {e}"));
            runs += 1;
        }
    }
    assert_eq!(runs, writes * 4);
    Totals { writes, runs }
}

#[allow(clippy::print_stdout)] // the sweeps print their counts as proof
fn report(name: &str, t: &Totals) {
    println!(
        "driver-path sweep, {name}: {} cut points x 4 torn variants = {} runs, all outcomes as required",
        t.writes, t.runs
    );
}

fn placed_of(img: &[u8]) -> Placed {
    place(&mut Dev::new(img.to_vec()), &test_layout()).unwrap()
}

/// Pi form, install of `c` (and of `d` over a bay that held `a`).
fn pi_install_sweep(name: &str, pre: &[u8], slot: &'static str, bay: u8, e: &Expect) {
    let layout = test_layout();
    let placed = placed_of(pre);
    let mut allowed = HashSet::new();
    set_sectors(&placed, &layout, bay, &mut allowed);
    let parts = pi_parts();
    let payloads = manifest(slot);
    let t = sweep(
        name,
        pre,
        &|dev| {
            let mut boot = Boot::on(dev).unwrap();
            let r = boot.install_slot(slot, &refs(&payloads)).map(|_| ());
            (boot.into_device(), r)
        },
        &allowed,
        &|img| pi_chain(img, &layout, e, &parts),
        &|img| {
            pi_chain(img, &layout, &Expect { ..*e }, &parts)?;
            let mut dev = Dev::new(img.to_vec());
            let placed = place(&mut dev, &layout)?;
            files_are(&mut dev, &placed, bay, slot)
        },
    );
    report(name, &t);
}

#[test]
fn sweep_pi_install_into_the_free_bay() {
    let h = history();
    pi_install_sweep(
        "Pi form, install c into the free bay",
        &h.h2,
        "c",
        2,
        &Expect {
            defaults: &[(1, "b")],
            previous: &[(0, "a")],
        },
    );
}

#[test]
fn sweep_pi_install_over_a_bay_that_holds_an_old_record() {
    // `a` was the previous and is not named any more: its bay is the one free bay, and its record and
    // files are still there. The install of `d` rewrites it.
    let h = history();
    pi_install_sweep(
        "Pi form, install d over the bay of a removed-from-protection slot",
        &h.h5,
        "d",
        0,
        &Expect {
            defaults: &[(2, "c")],
            previous: &[(1, "b")],
        },
    );
}

#[test]
fn sweep_pi_trial_arm() {
    let h = history();
    let layout = test_layout();
    let placed = placed_of(&h.h3);
    let mut allowed = HashSet::new();
    pair(placed.trial_pointer, &mut allowed);
    pair(placed.trial, &mut allowed);
    let parts = pi_parts();
    let e = Expect {
        defaults: &[(1, "b")],
        previous: &[(0, "a")],
    };
    let done = |img: &[u8]| {
        pi_chain(img, &layout, &e, &parts)?;
        let mut dev = Dev::new(img.to_vec());
        let placed = place(&mut dev, &layout)?;
        let t = pending_trial(&mut dev, placed.trial).map_err(|e| e.to_string())?;
        match t {
            Some(t) if t.slot == "c" && t.attempts == 3 => {}
            other => return Err(format!("the trial is {other:?}")),
        }
        if read_pointer(&mut dev, placed.trial_pointer).map_err(|e| e.to_string())? != Some(2) {
            return Err("the trial pointer does not name c's bay".into());
        }
        Ok(())
    };
    let t = sweep(
        "Pi form, trial arm",
        &h.h3,
        &|dev| {
            let mut boot = Boot::on(dev).unwrap();
            let r = boot.arm_trial("c", 3);
            (boot.into_device(), r)
        },
        &allowed,
        &|img| pi_chain(img, &layout, &e, &parts),
        &done,
    );
    report("Pi form, trial arm", &t);
}

#[test]
fn sweep_pi_confirm() {
    let h = history();
    let layout = test_layout();
    let placed = placed_of(&h.h4);
    let mut allowed = HashSet::new();
    pair(placed.default, &mut allowed);
    pair(placed.previous, &mut allowed);
    pair(placed.trial, &mut allowed);
    let parts = pi_parts();
    // The default is b before the switch and c after; the previous is a, then b.
    let e = Expect {
        defaults: &[(1, "b"), (2, "c")],
        previous: &[(0, "a"), (1, "b")],
    };
    let done = |img: &[u8]| {
        let end = Expect {
            defaults: &[(2, "c")],
            previous: &[(1, "b")],
        };
        pi_chain(img, &layout, &end, &parts)?;
        let mut dev = Dev::new(img.to_vec());
        let placed = place(&mut dev, &layout)?;
        if pending_trial(&mut dev, placed.trial)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("the trial is still pending".into());
        }
        Ok(())
    };
    let t = sweep(
        "Pi form, confirm",
        &h.h4,
        &|dev| {
            let mut boot = Boot::on(dev).unwrap();
            // The driver's order (resolve.rs): the default, then the trial's record.
            let r = boot
                .set_pointer(Pointer::Default, "c")
                .and_then(|()| boot.clear_trial());
            (boot.into_device(), r)
        },
        &allowed,
        &|img| pi_chain(img, &layout, &e, &parts),
        &done,
    );
    report("Pi form, confirm", &t);
}

#[test]
fn sweep_pi_remove() {
    let h = history();
    let layout = test_layout();
    let placed = placed_of(&h.h5);
    let mut allowed = HashSet::new();
    pair(placed.info[0], &mut allowed);
    let parts = pi_parts();
    let e = Expect {
        defaults: &[(2, "c")],
        previous: &[(1, "b")],
    };
    let done = |img: &[u8]| {
        pi_chain(img, &layout, &e, &parts)?;
        let mut boot = Boot::on(Dev::new(img.to_vec())).unwrap();
        if boot.bay_of("a").is_ok() {
            return Err("a still has a bay".into());
        }
        Ok(())
    };
    let t = sweep(
        "Pi form, remove",
        &h.h5,
        &|dev| {
            let mut boot = Boot::on(dev).unwrap();
            let r = boot.free_slot("a");
            (boot.into_device(), r)
        },
        &allowed,
        &|img| pi_chain(img, &layout, &e, &parts),
        &done,
    );
    report("Pi form, remove", &t);
    let _ = h.h0;
}

// ---- The U-Boot form: no pointers on the volume; the environment's bays are the caller's.

fn bay_name(set: u8) -> String {
    set_dir(set)
}

/// A U-Boot set's bootable reading: its record, every stored file against the manifest, and the
/// composed extlinux against the stored command line.
fn uboot_bay(dev: &mut Dev, set: u8, slot: &str) -> Result<(), String> {
    let layout = test_layout();
    let placed_sets = place_sets(dev, &layout)?;
    let placed = Placed {
        files: placed_sets.iter().map(|s| s.0).collect(),
        info: placed_sets.iter().map(|s| s.1).collect(),
        default: 0,
        previous: 0,
        trial: 0,
        trial_pointer: 0,
    };
    files_are(dev, &placed, set, slot)?;
    let want = bootimage::extlinux_text(&manifest(slot)[3])?;
    let found = bootfat::locate_path(
        dev,
        &format!("{}/{}", bay_name(set), bootimage::EXTLINUX),
        bootimage::EXTLINUX_BYTES,
    )
    .map_err(|e| e.to_string())?;
    let file =
        read_file(dev, found.first_lba, bootimage::EXTLINUX_BYTES).map_err(|e| e.to_string())?;
    if !(file.starts_with(&want) && file[want.len()..].iter().all(|&b| b == b' ')) {
        return Err(format!(
            "bay{set} ({slot}): extlinux.conf is not the command line's"
        ));
    }
    Ok(())
}

/// The environment's reading after a cut: its default's bay is bootable and whole, and so is the
/// previous's; `verify` accepts the image except for the set being written.
fn uboot_chain(img: &[u8], default: (u8, &str), previous: (u8, &str)) -> Result<(), String> {
    let mut dev = Dev::new(img.to_vec());
    uboot_bay(&mut dev, default.0, default.1).map_err(|e| format!("default: {e}"))?;
    uboot_bay(&mut dev, previous.0, previous.1).map_err(|e| format!("previous: {e}"))?;
    let layout = test_layout();
    let sets = place_sets(&mut dev, &layout)?;
    for set in 0..SETS {
        if let Some(info) = read_info(&mut dev, sets[set as usize].1).map_err(|e| e.to_string())? {
            uboot_bay(&mut dev, set, &info.slot)?;
        }
    }
    let bad: Vec<String> = bootimage::verify(&mut bootimage::Mem(img.to_vec()), &uboot_parts())
        .into_iter()
        .filter(|f| !f.ok)
        .map(|f| f.text)
        .collect();
    if !bad.is_empty() {
        return Err(format!("boot-partition --verify: {bad:?}"));
    }
    Ok(())
}

fn uboot_install(img: Vec<u8>, slot: &str, protected: &Protected) -> Vec<u8> {
    let p = manifest(slot);
    let text = bootimage::extlinux_text(&p[3]).unwrap();
    let mut v = BayVolume::on(Dev::new(img)).unwrap();
    v.install_slot_with(slot, &refs(&p), Some(&text), protected)
        .unwrap();
    v.into_device().img
}

fn prot(default: u8, previous: u8) -> Protected {
    Protected {
        default: Some(default),
        previous: Some(previous),
        trial: None,
        pending: None,
    }
}

fn uboot_set_sectors(img: &[u8], set: u8, with_extlinux: bool) -> HashSet<u64> {
    let layout = test_layout();
    let mut dev = Dev::new(img.to_vec());
    let sets = place_sets(&mut dev, &layout).unwrap();
    let placed = Placed {
        files: sets.iter().map(|s| s.0).collect(),
        info: sets.iter().map(|s| s.1).collect(),
        default: 0,
        previous: 0,
        trial: 0,
        trial_pointer: 0,
    };
    let mut out = HashSet::new();
    set_sectors(&placed, &layout, set, &mut out);
    if with_extlinux {
        let found = bootfat::locate_path(
            &mut dev,
            &format!("{}/{}", bay_name(set), bootimage::EXTLINUX),
            bootimage::EXTLINUX_BYTES,
        )
        .unwrap();
        for k in 0..bootimage::EXTLINUX_BYTES / SECTOR as u64 {
            out.insert(found.first_lba + k);
        }
    }
    out
}

#[test]
fn sweep_uboot_install() {
    // a in bay0 (previous), b in bay1 (default): c takes bay2.
    let img = uboot_install(uboot_image(), "b", &prot(0, 0));
    let pre = img;
    let allowed = uboot_set_sectors(&pre, 2, true);
    let p = manifest("c");
    let text = bootimage::extlinux_text(&p[3]).unwrap();
    let protected = prot(1, 0);
    let t = sweep(
        "U-Boot form, install c",
        &pre,
        &|dev| {
            let mut v = BayVolume::on(dev).unwrap();
            let r = v
                .install_slot_with("c", &refs(&p), Some(&text), &protected)
                .map(|_| ());
            (v.into_device(), r)
        },
        &allowed,
        &|img| uboot_chain(img, (1, "b"), (0, "a")),
        &|img| {
            uboot_chain(img, (1, "b"), (0, "a"))?;
            uboot_bay(&mut Dev::new(img.to_vec()), 2, "c")
        },
    );
    report("U-Boot form, install c", &t);
}

#[test]
fn sweep_uboot_install_over_a_bay_that_holds_an_old_record() {
    // a, b, c installed; default c (bay2), previous b (bay1): d takes bay0, which still holds a.
    let img = uboot_install(uboot_image(), "b", &prot(0, 0));
    let img = uboot_install(img, "c", &prot(1, 0));
    let allowed = uboot_set_sectors(&img, 0, true);
    let p = manifest("d");
    let text = bootimage::extlinux_text(&p[3]).unwrap();
    let protected = prot(2, 1);
    let t = sweep(
        "U-Boot form, install d over a's old bay",
        &img,
        &|dev| {
            let mut v = BayVolume::on(dev).unwrap();
            let r = v
                .install_slot_with("d", &refs(&p), Some(&text), &protected)
                .map(|_| ());
            (v.into_device(), r)
        },
        &allowed,
        &|img| uboot_chain(img, (2, "c"), (1, "b")),
        &|img| {
            uboot_chain(img, (2, "c"), (1, "b"))?;
            uboot_bay(&mut Dev::new(img.to_vec()), 0, "d")
        },
    );
    report("U-Boot form, install d over a's old bay", &t);
}

#[test]
fn sweep_uboot_remove() {
    let img = uboot_install(uboot_image(), "b", &prot(0, 0));
    let img = uboot_install(img, "c", &prot(1, 0));
    let layout = test_layout();
    let mut dev = Dev::new(img.clone());
    let sets = place_sets(&mut dev, &layout).unwrap();
    let mut allowed = HashSet::new();
    pair(sets[0].1, &mut allowed);
    let protected = prot(2, 1);
    let t = sweep(
        "U-Boot form, remove a",
        &img,
        &|dev| {
            let mut v = BayVolume::on(dev).unwrap();
            let r = v.free_slot("a", &protected);
            (v.into_device(), r)
        },
        &allowed,
        &|img| uboot_chain(img, (2, "c"), (1, "b")),
        &|img| {
            uboot_chain(img, (2, "c"), (1, "b"))?;
            let mut v = BayVolume::on(Dev::new(img.to_vec())).unwrap();
            match v.slot_bay("a") {
                Err(_) => Ok(()),
                Ok(_) => Err("a still has a bay".into()),
            }
        },
    );
    report("U-Boot form, remove a", &t);
}
