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
        armed: true,
    };
    let text = record.text();
    assert_eq!(text, "default=os-1\ntrial=os-2\ntrial_armed=1\n");
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
    assert_eq!(none.text(), "default=os-1\ntrial=\ntrial_armed=0\n");
    assert_eq!(Record::parse(&none.text()), none);
    // What `env export -t` writes back has the same lines; an unset trial is left out.
    assert_eq!(Record::parse("default=os-1\ntrial_armed=0\n"), none);
    // A value that isn't a slot id is no slot; a stray line is skipped.
    assert_eq!(
        Record::parse("#c\ndefault=../x\nother=1\ntrial=os-2\n").default,
        ""
    );
}

#[test]
fn the_decision_table() {
    let armed = Record {
        default: "a".into(),
        trial: "b".into(),
        armed: true,
    };
    assert_eq!(
        steps(Some(&armed)),
        [
            Step::Disarm,
            Step::Boot("b".into()),
            Step::Boot("a".into()),
            Step::Root
        ]
    );
    // The trial's slot missing: its sysboot returns and the default follows, as listed.
    let not_armed = Record {
        armed: false,
        ..armed.clone()
    };
    assert_eq!(
        steps(Some(&not_armed)),
        [Step::Boot("a".into()), Step::Root]
    );
    let armed_without_trial = Record {
        trial: String::new(),
        ..armed.clone()
    };
    assert_eq!(
        steps(Some(&armed_without_trial)),
        [Step::Boot("a".into()), Step::Root]
    );
    assert_eq!(steps(None), [Step::Root]);
    assert_eq!(steps(Some(&Record::default())), [Step::Root]);
}

/// The script against [`steps`]: the same keys, the same order, the disarm written before the
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
    let armed_test = at(r#"test "${trial_armed}" = "1" && test -n "${trial}""#);
    let disarm = at("setenv trial_armed 0");
    let export = at("env export -t");
    let save = at(&format!(
        "save ${{devtype}} ${{slot_part}} ${{scriptaddr}} {RECORD_FILE} ${{filesize}}"
    ));
    let trial = at(&format!("/slots/${{trial}}/{EXTLINUX}"));
    let default = at(&format!("/slots/${{default}}/{EXTLINUX}"));
    let root = SCRIPT.rfind("/extlinux/extlinux.conf").unwrap();
    assert!(armed_test < disarm && disarm < export && export < save && save < trial);
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
            systemctl: "/nonexistent/systemctl".into(),
        },
        volume: None,
    };
    record.set_default("os-1").unwrap();
    record.set_trial("os-2").unwrap();
    assert_eq!(
        fs::read_to_string(layout.boot.join(RECORD_FILE)).unwrap(),
        "default=os-1\ntrial=os-2\ntrial_armed=1\n"
    );
    record.set_default("os-2").unwrap();
    assert_eq!(
        record.read(),
        Record {
            default: "os-2".into(),
            trial: "os-2".into(),
            armed: true
        }
    );
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
    let armed_test = at(r#"test "${trial_armed}" = "1" && test -n "${trial}""#);
    let disarm = at("setenv trial_armed 0");
    let export = at(&format!("env export -t ${{scriptaddr}} {}", KEYS.join(" ")));
    let write = at(&format!(
        "ubi write ${{scriptaddr}} {RECORD_VOLUME} ${{filesize}}"
    ));
    let trial = at(&format!("/slots/${{trial}}/{EXTLINUX}"));
    let default = at(&format!("/slots/${{default}}/{EXTLINUX}"));
    let root = SCRIPT_UBI.rfind("/extlinux/extlinux.conf").unwrap();
    assert!(mount < read && read < import && import < armed_test);
    assert!(armed_test < disarm && disarm < export && export < write && write < trial);
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
    // The decision table is the same one the file record's script follows.
    assert_eq!(
        SCRIPT_UBI.matches("sysboot").count(),
        SCRIPT.matches("sysboot").count()
    );
}
