//! The `uboot` record: its text form, the loader configuration, the driver's writes, and the
//! loader script's text against the decision it mirrors. The script itself is not run here.

use std::fs;

use super::*;
use crate::tests::TempDir;

const SCRIPT: &str = include_str!("../../../../packaging/uboot/boot.cmd");
const SCRIPT_UBI: &str = include_str!("../../../../packaging/uboot/boot-ubi.cmd");

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn the_record_round_trips_in_the_form_env_import_takes() {
    let record = Record {
        default: "os-1".into(),
        trial: "os-2".into(),
        attempts: 3,
    };
    let text = record.text();
    assert_eq!(text, "default=os-1\ntrial=os-2\ntrial_attempts=3\n");
    assert_eq!(Record::parse(&text), record);
    // Every line is `name=value` with a key the script imports: what `env import -t` reads.
    for line in text.lines() {
        let (key, value) = line.split_once('=').unwrap();
        assert!(KEYS.contains(&key), "{key}");
        assert!(!value.contains([' ', '\t', '=', '\\']), "{line}");
    }
    let none = Record {
        default: "os-1".into(),
        ..Record::default()
    };
    assert_eq!(none.text(), "default=os-1\ntrial=\ntrial_attempts=0\n");
    assert_eq!(Record::parse(&none.text()), none);
    // What `env export -t` writes back has the same lines; an unset trial is left out.
    assert_eq!(Record::parse("default=os-1\ntrial_attempts=0\n"), none);
    // A value that isn't a slot id is no slot; a stray line is skipped.
    assert_eq!(
        Record::parse("#c\ndefault=../x\nother=1\ntrial=os-2\n").default,
        ""
    );
}

#[test]
fn the_decision_table() {
    let tried = Record {
        default: "a".into(),
        trial: "b".into(),
        attempts: 3,
    };
    // Each attempt the record holds takes one before the trial starts; the default follows.
    assert_eq!(
        steps(Some(&tried)),
        [
            Step::Trial {
                id: "b".into(),
                attempts: 2
            },
            Step::Boot("a".into()),
            Step::Root
        ]
    );
    let last = Record {
        attempts: 1,
        ..tried.clone()
    };
    assert_eq!(
        steps(Some(&last)),
        [
            Step::Trial {
                id: "b".into(),
                attempts: 0
            },
            Step::Boot("a".into()),
            Step::Root
        ]
    );
    // Out of attempts, the trial is not started at all.
    let out = Record {
        attempts: 0,
        ..tried.clone()
    };
    assert_eq!(steps(Some(&out)), [Step::Boot("a".into()), Step::Root]);
    let no_trial = Record {
        trial: String::new(),
        ..tried.clone()
    };
    assert_eq!(steps(Some(&no_trial)), [Step::Boot("a".into()), Step::Root]);
    assert_eq!(steps(None), [Step::Root]);
    assert_eq!(steps(Some(&Record::default())), [Step::Root]);
}

/// Whether `script` takes the rung for `count`: its `if` for that count is followed by setting
/// `left` as the next step (the first line after it that isn't empty).
fn rung(script: &str, count: u8, left: u8) -> bool {
    let want = format!("test \"${{trial_attempts}}\" = \"{count}\"; then");
    let mut lines = script.lines().skip_while(|l| {
        !(l.trim().starts_with("if ") || l.trim().starts_with("elif "))
            || !l.trim().ends_with(&want)
    });
    lines.next().is_some() && lines.next().map(str::trim) == Some(&format!("setenv left {left}"))
}

/// The ladder both scripts share: 3 to 2, 2 to 1, 1 to 0 with `test` and `setenv` only, and
/// nothing at 0 (the default starts).
fn ladder_is_there(script: &str) {
    assert!(rung(script, 3, 2), "3 takes one to 2");
    assert!(rung(script, 2, 1), "2 takes one to 1");
    assert!(rung(script, 1, 0), "1 takes one to 0");
    assert!(
        !script.contains("setexpr"),
        "no setexpr: U-Boot builds without it"
    );
    assert!(!rung(script, 0, 0), "nothing is taken at 0");
}

/// The script against [`steps`]: the same keys, the same order, the attempt written before the
/// trial starts, and no fixed device numbers.
#[test]
fn the_script_mirrors_the_decision() {
    let at = |needle: &str| {
        SCRIPT
            .find(needle)
            .unwrap_or_else(|| panic!("the script has no `{needle}`"))
    };
    assert!(SCRIPT.contains(&format!(
        "env import -t ${{scriptaddr}} ${{filesize}} {}",
        KEYS.join(" ")
    )));
    assert!(SCRIPT.contains(&format!("env export -t ${{scriptaddr}} {}", KEYS.join(" "))));
    assert!(SCRIPT.contains(&format!(
        "load ${{devtype}} ${{slot_part}} ${{scriptaddr}} {RECORD_FILE}"
    )));
    ladder_is_there(SCRIPT);
    let ladder = at(r#"if test "${trial_attempts}" = "3"; then"#);
    let take = at("setenv trial_attempts ${left}");
    let export = at("env export -t");
    let save = at(&format!(
        "save ${{devtype}} ${{slot_part}} ${{scriptaddr}} {RECORD_FILE} ${{filesize}}"
    ));
    let trial = at(&format!("/slots/${{trial}}/{EXTLINUX}"));
    let default = at(&format!("/slots/${{default}}/{EXTLINUX}"));
    let root = SCRIPT.rfind("/extlinux/extlinux.conf").unwrap();
    assert!(ladder < take && take < export && export < save && save < trial);
    assert!(trial < default && default < root);
    assert!(
        SCRIPT
            .lines()
            .last()
            .unwrap()
            .contains("any ${pxefile_addr_r} /extlinux/extlinux.conf")
    );
    // The device comes from the standard boot flow, never a number written in.
    for line in SCRIPT.lines().filter(|l| !l.trim_start().starts_with('#')) {
        for fixed in ["mmc 0", "mmc 1", "usb 0", "0:1", "1:1"] {
            assert!(!line.contains(fixed), "{line}");
        }
    }
    assert!(SCRIPT.contains("setenv slot_part ${devnum}:${distro_bootpart}"));
    // Nothing saved in U-Boot's own environment, no boot count.
    for banned in ["saveenv", "bootcount", "bootlimit"] {
        assert!(!SCRIPT.contains(banned), "{banned}");
    }
}

#[test]
fn extlinux_names_the_slots_files_by_absolute_path() {
    let conf = extlinux(
        "os-1",
        &files(&["Image", "board-a.dtb", "initramfs", "cmdline.txt"]),
        "console=ttyS0 root=x b.slot=os-1",
    )
    .unwrap();
    assert_eq!(
        conf,
        "# Written by the board's slot driver for slot os-1. Rewritten at each install.\n\
         default os-1\n\
         timeout 0\n\
         label os-1\n\
         \x20 kernel /slots/os-1/Image\n\
         \x20 fdt /slots/os-1/board-a.dtb\n\
         \x20 initrd /slots/os-1/initramfs\n\
         \x20 append console=ttyS0 root=x b.slot=os-1\n"
    );
}

#[test]
fn several_device_trees_go_through_fdtdir() {
    let top = extlinux(
        "os-1",
        &files(&[
            "kernel8.img",
            "a.dtb",
            "b.dtb",
            "initramfs",
            "overlays/x.dtbo",
        ]),
        "x",
    )
    .unwrap();
    assert!(top.contains("  kernel /slots/os-1/kernel8.img\n"));
    assert!(top.contains("  fdtdir /slots/os-1/\n"));
    let sub = extlinux(
        "os-1",
        &files(&["vmlinuz-6.12", "initrd.img", "dtbs/vendor/a.dtb"]),
        "x",
    )
    .unwrap();
    assert!(sub.contains("  kernel /slots/os-1/vmlinuz-6.12\n"));
    assert!(sub.contains("  fdtdir /slots/os-1/dtbs/\n"));
    assert!(sub.contains("  initrd /slots/os-1/initrd.img\n"));
}

#[test]
fn what_cant_go_into_the_configuration_is_refused() {
    assert!(
        extlinux("os-1", &files(&["initramfs"]), "x")
            .unwrap_err()
            .contains("kernel")
    );
    assert!(
        extlinux("os-1", &files(&["Image"]), "x")
            .unwrap_err()
            .contains("initramfs")
    );
    assert!(extlinux("../x", &files(&["Image", "initramfs"]), "x").is_err());
    for bad in ["my board.dtb", "a#b.dtb", "a\"b.dtb"] {
        let err = extlinux("os-1", &files(&["Image", "initramfs", bad]), "x").unwrap_err();
        assert!(err.contains("can't go into"), "{bad}: {err}");
    }
    assert!(extlinux("os-1", &files(&["Image", "initramfs"]), "a\nb").is_err());
}

#[test]
fn the_driver_writes_the_record_whole_and_keeps_the_root_copy() {
    let dir = TempDir::new("uboot");
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    fs::create_dir_all(layout.boot_dir("os-1")).unwrap();
    let mut record = Uboot {
        boot: layout.boot.clone(),
        restarter: Restarter {
            check: None,
            systemctl: "/nonexistent/systemctl".into(),
        },
        volume: None,
    };
    record.set_default("os-1").unwrap();
    record.set_trial("os-2", 3).unwrap();
    assert_eq!(
        fs::read_to_string(layout.boot.join(RECORD_FILE)).unwrap(),
        "default=os-1\ntrial=os-2\ntrial_attempts=3\n"
    );
    record.set_default("os-2").unwrap();
    assert_eq!(
        record.read(),
        Record {
            default: "os-2".into(),
            trial: "os-2".into(),
            attempts: 3
        }
    );
    // The attempts are the trial's own: set, read back, and cleared with it.
    record.set_attempts(1).unwrap();
    assert_eq!(record.pending().unwrap(), Some(("os-2".into(), 1)));
    record.clear_trial().unwrap();
    assert_eq!(record.pending().unwrap(), None);
    assert_eq!(record.read().attempts, 0);
    record.set_trial("os-2", 3).unwrap();
    // Written beside and renamed: nothing else is left in the directory.
    let names: Vec<String> = fs::read_dir(&layout.boot)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "slots")
        .collect();
    assert_eq!(names, [RECORD_FILE]);
    // The slot's configuration, prepared at install, and its copy at the root.
    let slot = layout.boot_dir("os-1");
    fs::write(slot.join("Image"), "k").unwrap();
    fs::write(slot.join("initramfs"), "i").unwrap();
    record.prepare(&slot, "os-1", "b.slot=os-1").unwrap();
    record.fallback_copy(&layout, "os-1").unwrap();
    assert_eq!(
        fs::read(layout.boot.join(EXTLINUX)).unwrap(),
        fs::read(slot.join(EXTLINUX)).unwrap()
    );
    // No kernel at the root: U-Boot reads it from the slot's directory.
    assert!(!layout.boot.join("Image").exists());
}

#[test]
fn the_nand_script_mirrors_the_same_decision_over_the_record_volume() {
    use lemnos_slot_store::ubi::{BOOT_VOLUME, RECORD_VOLUME};
    let at = |needle: &str| {
        SCRIPT_UBI
            .find(needle)
            .unwrap_or_else(|| panic!("the script has no `{needle}`"))
    };
    let mount = at(&format!("ubifsmount ubi0:{BOOT_VOLUME}"));
    let read = at(&format!("ubi read ${{scriptaddr}} {RECORD_VOLUME}"));
    let import = at(&format!(
        "env import -t ${{scriptaddr}} ${{filesize}} {}",
        KEYS.join(" ")
    ));
    ladder_is_there(SCRIPT_UBI);
    let ladder = at(r#"if test "${trial_attempts}" = "3"; then"#);
    let export = at(&format!("env export -t ${{scriptaddr}} {}", KEYS.join(" ")));
    let write = at(&format!(
        "ubi write ${{scriptaddr}} {RECORD_VOLUME} ${{filesize}}"
    ));
    let trial = at(&format!("/slots/${{trial}}/{EXTLINUX}"));
    let default = at(&format!("/slots/${{default}}/{EXTLINUX}"));
    let root = SCRIPT_UBI.rfind("/extlinux/extlinux.conf").unwrap();
    assert!(mount < read && read < import && import < ladder);
    assert!(ladder < export && export < write && write < trial);
    assert!(trial < default && default < root);
    // Every boot goes through the UBIFS boot volume, and the last resort is the root copy.
    for line in SCRIPT_UBI
        .lines()
        .filter(|l| l.trim_start().starts_with("sysboot"))
    {
        assert!(
            line.contains(&format!(
                "sysboot ubi 0:{BOOT_VOLUME} any ${{pxefile_addr_r}} "
            )),
            "{line}"
        );
    }
    assert!(
        SCRIPT_UBI
            .lines()
            .last()
            .unwrap()
            .ends_with(" /extlinux/extlinux.conf")
    );
    assert!(SCRIPT_UBI.contains("ubi part ${ubi_mtd}"));
    for banned in ["saveenv", "bootcount", "bootlimit", "mmc ", "load "] {
        assert!(!SCRIPT_UBI.contains(banned), "{banned}");
    }
    // The two scripts take the same steps in the same order.
    assert_eq!(
        SCRIPT_UBI.matches("sysboot").count(),
        SCRIPT.matches("sysboot").count()
    );
}
