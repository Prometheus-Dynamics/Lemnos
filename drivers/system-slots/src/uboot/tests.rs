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
    ladder_is_there(SCRIPT);
    let ladder = at(r#"if test "${trial_attempts}" = "3"; then"#);
    let take = at("setenv trial_attempts ${left}");
    let after = |from: usize, needle: &str| {
        from + SCRIPT[from..]
            .find(needle)
            .unwrap_or_else(|| panic!("the script has no `{needle}` after the ladder"))
    };
    let save = after(take, "if saveenv; then");
    let taken = after(save, "setenv take_trial ${trial}");
    let trial = at("/slots/${take_trial_bay}/");
    let default = at("/slots/${take_default_bay}/");
    let previous = at("/slots/${take_previous_bay}/");
    assert!(ladder < take && take < save && save < taken && taken < trial);
    assert!(trial < default && default < previous);
    // One loop over the choices, in this order, with one sysboot. The block script has no root copy
    // since I5i8: the bays hold every slot's files.
    assert!(SCRIPT.contains("for step in trial default previous; do"));
    // Each step reads the device tree and the loader file from the bay the record names, never from a
    // directory named for the slot (the slot's files are in a bay, not under its id).
    for who in ["trial", "default", "previous"] {
        for file in ["board.dtb", "extlinux/extlinux.conf"] {
            let line = format!("/slots/${{take_{who}_bay}}/{file}");
            assert!(SCRIPT.contains(&format!("{line}\n")), "{line}");
            assert!(!SCRIPT.contains(&format!("/slots/${{take_{who}}}/")));
        }
    }
    assert_eq!(sysboots(SCRIPT), 1);
    assert!(SCRIPT.contains("any ${pxefile_addr_r} ${lemnos_conf}"));
    // The device comes from the standard boot flow, never a number written in.
    // (`setenv lemnos_usb 0` is the default of the USB controller index the fastboot offer takes.)
    for line in SCRIPT
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.contains("setenv lemnos_usb"))
    {
        for fixed in ["mmc 0", "mmc 1", "usb 0", "0:1", "1:1"] {
            assert!(!line.contains(fixed), "{line}");
        }
    }
    assert!(SCRIPT.contains("setenv slot_part ${devnum}:${distro_bootpart}"));
    boot_source_steps(SCRIPT);
    // The record is U-Boot's environment: no FAT load or save of it, no import or export, and no
    // boot count. The environment is saved twice: the first one, and the trial's attempt.
    let code: Vec<&str> = SCRIPT
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    for banned in [
        "env import",
        "env export",
        "boot-record.env",
        "save ${devtype}",
        "bootcount",
        "bootlimit",
    ] {
        assert!(!code.iter().any(|l| l.contains(banned)), "{banned}");
    }
    assert!(
        !code
            .iter()
            .any(|l| l.starts_with("load ") && l.contains("scriptaddr")),
        "no record is loaded from the boot partition"
    );
    assert_eq!(
        code.iter().filter(|l| l.contains("saveenv")).count(),
        2,
        "the first environment and the trial's attempt"
    );
}

/// The previous slot's step: third after the trial and the default, started as a trial, and writing
/// nothing to the stored environment. The script is structural only (no U-Boot is run here).
#[test]
fn the_previous_step_is_the_third_and_writes_nothing_to_the_environment() {
    let code: Vec<&str> = SCRIPT
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    let at = |needle: &str| {
        code.iter()
            .position(|l| l.starts_with(needle))
            .unwrap_or_else(|| panic!("the script has no `{needle}`"))
    };
    // Three steps, in order: a trial with an attempt, the default, the previous.
    assert_eq!(
        code.iter()
            .filter(|l| l.starts_with("for step in "))
            .count(),
        1
    );
    assert!(code.contains(&"for step in trial default previous; do"));
    let trial = at("if test \"${step}\" = \"trial\"; then");
    let default = at("elif test \"${step}\" = \"default\"; then");
    let previous = at("setenv lemnos_dtb /slots/${take_previous_bay}/board.dtb");
    assert!(trial < default && default < previous);
    // Named only when there is a previous slot, it has a bay, it is not the default, and it is not the
    // trial the first step already tried; otherwise the step is skipped (take_previous stays empty).
    let guards = [
        r#"if test -n "${previous}"; then"#,
        r#"if test -n "${previous_bay}"; then"#,
        r#"if test "${previous}" != "${default}"; then"#,
        r#"if test "${previous}" != "${take_trial}"; then"#,
        "setenv take_previous ${previous}",
        "setenv take_previous_bay ${previous_bay}",
    ];
    let mut last = 0;
    for guard in guards {
        let line = at(guard);
        assert!(line > last, "{guard}");
        last = line;
    }
    assert!(last < at("for step in"));
    // It starts the previous bay's own files through the same one sysboot as the others.
    assert!(
        code.contains(&"setenv lemnos_conf /slots/${take_previous_bay}/extlinux/extlinux.conf")
    );
    assert_eq!(sysboots(SCRIPT), 1);
    // It writes nothing to the stored environment: both saveenv are before the steps (the first
    // environment and the trial's attempt), and no step sets a stored key. The previous step's only
    // marking is a property of the device tree it hands on, set when the default had failed.
    let steps = at("for step in");
    for (i, line) in code.iter().enumerate() {
        if line.contains("saveenv") {
            assert!(i < steps, "saveenv inside the steps: {line}");
        }
        if i > steps {
            for key in [
                "default",
                "default_bay",
                "previous",
                "previous_bay",
                "trial",
                "trial_bay",
                "trial_attempts",
            ] {
                assert!(
                    !line.starts_with(&format!("setenv {key} "))
                        && *line != format!("setenv {key}"),
                    "a step sets the stored key {key}: {line}"
                );
            }
        }
    }
    let marked = at("fdt set /chosen lemnos,default-failed");
    assert!(previous < marked);
    assert_eq!(
        code[marked],
        r#"fdt set /chosen lemnos,default-failed "${lemnos_dflt}""#
    );
    // The reason is for the previous step only, and only when the default step had a slot to start.
    assert!(
        code[..marked]
            .iter()
            .rev()
            .take(4)
            .any(|l| *l == r#"if test "${step}" = "previous"; then"#)
    );
    assert!(code.contains(&"setenv lemnos_dflt files"));
    assert!(at("setenv lemnos_dflt files") < previous);
    // Each pass starts with the reason cleared, before the trial's saveenv could store it.
    let clear = at("setenv lemnos_dflt");
    assert_eq!(code[clear], "setenv lemnos_dflt");
    assert!(
        code[clear..]
            .iter()
            .any(|l| l.starts_with("if saveenv; then")),
        "the trial's saveenv follows the clear"
    );
}

/// The `sysboot` commands of a script (comments left out).
fn sysboots(script: &str) -> usize {
    script
        .lines()
        .filter(|l| l.trim_start().starts_with("sysboot"))
        .count()
}

/// The boot-source steps: the tree is resized before the property is set, every `load` is a
/// condition (a failed one is reported and the next choice follows), and the only `exit` is the
/// one place where the boot source cannot be named.
fn boot_source_steps(script: &str) {
    let code: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    let at = |needle: &str| {
        code.iter()
            .position(|l| l.starts_with(needle))
            .unwrap_or_else(|| panic!("the script has no `{needle}`"))
    };
    let addr = at("fdt addr ${fdt_addr_r}");
    let resize = at("fdt resize ");
    let set = at("if fdt set /chosen lemnos,boot-source");
    assert!(addr < resize && resize < set, "resize between addr and set");
    assert!(
        code[resize]
            .split(' ')
            .nth(2)
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
    for line in code.iter().filter(|l| l.contains("load ${devtype}")) {
        assert!(line.starts_with("if "), "an unchecked load: {line}");
    }
    // The script never ends at a prompt: no `exit` anywhere, and nothing is left after the loop.
    assert!(
        !code.iter().any(|l| *l == "exit" || l.starts_with("exit ")),
        "an exit would end at a prompt"
    );
    assert!(!script.contains("replaced by the download-mode loop"));
    assert_eq!(code.last(), Some(&"done"), "the loop ends the script");
    // One `while`, set to run for ever: lemnos_go is only ever set to 1, and never cleared.
    let whiles: Vec<&&str> = code.iter().filter(|l| l.starts_with("while ")).collect();
    assert_eq!(whiles, [&r#"while test "${lemnos_go}" = "1"; do"#]);
    for line in code.iter().filter(|l| l.contains("lemnos_go")) {
        assert!(
            line.starts_with("setenv lemnos_go 1") || line.starts_with("while "),
            "{line}"
        );
    }
    // Every pass ends the same way, at the loop's own level (one tab, not inside an `if`): the
    // counter grows by one, the third pass in a row that started nothing offers download mode (the
    // fastboot offer, LED around it, then the counter goes back to two so that the next pass tries
    // the slots once before offering again), the others say they try again, and a sleep ends the pass.
    let level_one: Vec<&str> = script
        .lines()
        .filter(|l| l.starts_with('\t') && !l.starts_with("\t\t") && !l.starts_with("\t#"))
        .collect();
    assert!(
        level_one.contains(&"\tsetenv lemnos_pass ${lemnos_pass}x"),
        "{level_one:?}"
    );
    assert!(
        level_one.contains(&"\tif test \"${lemnos_pass}\" = \"xxx\"; then"),
        "{level_one:?}"
    );
    assert_eq!(
        level_one.last(),
        Some(&"\tsleep 2"),
        "the pass ends in a sleep, not a busy loop"
    );
    let pass = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("no `{needle}`"))
    };
    let offer = pass("\t\tfastboot usb ${lemnos_usb}");
    assert!(pass("\tif test \"${lemnos_pass}\" = \"xxx\"; then") < offer);
    assert!(
        offer < pass("\t\tsetenv lemnos_pass xx\n"),
        "the counter backs off after the offer"
    );
    assert!(pass("\telse\n\t\techo \"lemnos: no slot started (pass") > offer);
    assert_eq!(
        code.iter().filter(|l| l.starts_with("fastboot ")).count(),
        1,
        "one place offers download mode"
    );
    // The offer comes after the `for` over the slots and after the refusal branch: a failed source
    // and a failed boot reach the same offer.
    let at_line = |needle: &str| code.iter().position(|l| l.starts_with(needle)).unwrap();
    assert!(at_line("for step in") < at_line("fastboot usb"));
    assert!(at_line("echo \"lemnos: ${lemnos_why}, starting no slot\"") < at_line("fastboot usb"));
    // The wait is not a busy loop: a sleep follows the offer.
    assert!(at_line("fastboot usb") < at_line("sleep 2"));
}

#[test]
fn extlinux_names_the_slots_files_by_absolute_path() {
    let conf = extlinux(
        "os-1",
        &files(&["Image", "board.dtb", "initramfs", "cmdline.txt"]),
        "console=ttyS0 root=x b.slot=os-1",
    )
    .unwrap();
    // The label has no fdt line: the boot script loads board.dtb and names the boot source first.
    assert_eq!(
        conf,
        "# Written by the board's slot driver for slot os-1. Rewritten at each install.\n\
         default os-1\n\
         timeout 0\n\
         label os-1\n\
         \x20 kernel /slots/os-1/Image\n\
         \x20 initrd /slots/os-1/initramfs\n\
         \x20 append console=ttyS0 root=x b.slot=os-1\n"
    );
}

#[test]
fn a_slot_without_board_dtb_is_refused_and_no_other_dtb_is_used() {
    let top = extlinux(
        "os-1",
        &files(&[
            "kernel8.img",
            "a.dtb",
            "board.dtb",
            "initramfs",
            "overlays/x.dtbo",
        ]),
        "x",
    )
    .unwrap();
    assert!(top.contains("  kernel /slots/os-1/kernel8.img\n"));
    assert!(!top.contains("fdt"), "{top}");
    let err = extlinux(
        "os-1",
        &files(&["vmlinuz-6.12", "initrd.img", "dtbs/vendor/board.dtb"]),
        "x",
    )
    .unwrap_err();
    assert!(err.contains("no board.dtb"), "{err}");
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
    for bad in ["kernel#1.img", "kernel\"1.img"] {
        let err = extlinux("os-1", &files(&[bad, "initramfs", "board.dtb"]), "x").unwrap_err();
        assert!(err.contains("can't go into"), "{bad}: {err}");
    }
    assert!(
        extlinux("os-1", &files(&["Image", "initramfs"]), "x")
            .unwrap_err()
            .contains("board.dtb")
    );
    assert!(extlinux("os-1", &files(&["Image", "initramfs"]), "a\nb").is_err());
}

#[test]
fn the_file_form_is_refused_by_name_and_the_environment_record_round_trips() {
    let dir = TempDir::new("uboot-env");
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    fs::create_dir_all(layout.boot_dir("os-1")).unwrap();
    let restarter = || Restarter {
        check: None,
        systemctl: "/nonexistent/systemctl".into(),
    };
    // The file form (the record file on the boot partition) writes nothing at run time.
    let mut file = Uboot {
        boot: layout.boot.clone(),
        restarter: restarter(),
        volume: None,
    };
    let err = file.set_default("os-1").unwrap_err();
    assert!(err.to_string().contains("not written at run time"), "{err}");
    assert!(!layout.boot.join(RECORD_FILE).exists());

    // The environment record: a disk image (GPT, the environment partition among others) holding
    // a valid environment at the top of that partition, changed through the record.
    let node = dir.0.join("disk.img");
    let mut image = lemnos_slot_store::gpt::image(
        &[
            ([1; 16], 34, 99),
            (lemnos_slot_store::gpt::ENV_TYPE_BYTES, 100, 227),
        ],
        256,
    );
    let place = Place::gpt(1024).unwrap();
    let geometry = {
        let mut dev = lemnos_slot_store::uboot_env::FileEnv::open_sized({
            fs::write(&node, &image).unwrap();
            &node
        })
        .unwrap();
        place.geometry(&mut dev.0, dev.1).unwrap()
    };
    let list = vec![("default".to_string(), "os-1".to_string())];
    let data = lemnos_slot_store::uboot_env::encode_vars(&list, geometry.data_len()).unwrap();
    let at = geometry.offset as usize;
    image[at..at + 1024].copy_from_slice(&lemnos_slot_store::uboot_env::encode_copy(&data, 1));
    fs::write(&node, &image).unwrap();
    // The boot partition, built by the image builder: os-1 in bay 0.
    let boot_node = dir.0.join("boot.img");
    fs::write(&boot_node, crate::bay_fixture::uboot_image("os-1")).unwrap();
    let mut env = UbootEnv {
        disk: Ok(node.clone()),
        place: place.clone(),
        restarter: restarter(),
        boot_node: Ok(boot_node),
    };
    let current = || {
        let mut dev = lemnos_slot_store::uboot_env::FileEnv::open(&node).unwrap();
        lemnos_slot_store::uboot_env::load(&mut dev, &geometry)
            .unwrap()
            .unwrap()
            .1
    };
    assert_eq!(env.pending().unwrap(), None);
    // The environment names os-1's bay as the default, then os-2's, and the bay the default had is
    // kept as the previous: the installs take the bays nothing names.
    let install = |env: &UbootEnv, id: &str| {
        let p = crate::bay_fixture::payloads(id);
        env.install(id, &crate::bay_fixture::refs(&p)).unwrap();
    };
    env.set_default("os-1").unwrap();
    install(&env, "os-2");
    env.set_default("os-2").unwrap();
    install(&env, "os-3");
    // The previous (os-1, bay 0) was not taken by the third install.
    assert_eq!(env.slot_bay("os-1").unwrap().0, "slots/bay0");
    assert_eq!(env.slot_bay("os-2").unwrap().0, "slots/bay1");
    assert!(current().contains(&("default".to_string(), "os-2".to_string())));
    env.set_trial("os-3", 3).unwrap();
    assert_eq!(env.pending().unwrap(), Some(("os-3".to_string(), 3)));
    env.set_attempts(2).unwrap();
    assert_eq!(env.pending().unwrap(), Some(("os-3".to_string(), 2)));
    // A trial with no attempts left is not pending, but its bay (`trial_bay`) is still named: a fourth
    // install finds every bay named (default bay 1, previous bay 0, trial bay 2) and writes nothing.
    env.set_attempts(0).unwrap();
    assert_eq!(env.pending().unwrap(), Some(("os-3".to_string(), 0)));
    let before = fs::read(env.boot_node.as_ref().unwrap()).unwrap();
    let p = crate::bay_fixture::payloads("os-4");
    let why = env
        .install("os-4", &crate::bay_fixture::refs(&p))
        .unwrap_err()
        .to_string();
    assert!(why.contains("every bay set is named"), "{why}");
    assert_eq!(fs::read(env.boot_node.as_ref().unwrap()).unwrap(), before);
    assert_eq!(env.slot_bay("os-3").unwrap().0, "slots/bay2");
    env.clear_trial().unwrap();
    assert_eq!(env.pending().unwrap(), None);
    // The root copy is install-time only: nothing is written to the boot partition here.
    let slot = layout.boot_dir("os-1");
    fs::write(slot.join("Image"), "k").unwrap();
    fs::write(slot.join("initramfs"), "i").unwrap();
    fs::write(slot.join("board.dtb"), "d").unwrap();
    env.prepare(&slot, "os-1", "b.slot=os-1").unwrap();
    env.fallback_copy(&layout, "os-1").unwrap();
    assert!(!layout.boot.join(EXTLINUX).exists());
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
    assert_eq!(sysboots(SCRIPT_UBI), 3);
    assert_eq!(sysboots(SCRIPT), 1);
}

#[test]
fn the_environment_record_refuses_by_name_when_the_disk_or_the_environment_is_not_there() {
    let dir = TempDir::new("uboot-env-refuse");
    let restarter = || Restarter {
        check: None,
        systemctl: "/nonexistent/systemctl".into(),
    };
    let gpt_disk = |name: &str, parts: &[([u8; 16], u64, u64)]| {
        let node = dir.0.join(name);
        fs::write(&node, lemnos_slot_store::gpt::image(parts, 256)).unwrap();
        node
    };
    let env = |disk: Result<PathBuf, String>, size: usize| UbootEnv {
        disk,
        place: Place::gpt(size).unwrap(),
        restarter: restarter(),
        boot_node: Err("not needed: the environment only".into()),
    };
    // No boot disk is known.
    let mut record = env(
        Err("no boot_sources entry names the controller".into()),
        1024,
    );
    let err = record.set_default("a").unwrap_err().to_string();
    assert!(err.contains("the boot disk isn't known"), "{err}");
    assert!(record.pending().is_err());
    // No partition of the environment's type, then two of them.
    let none = gpt_disk("none.img", &[([1; 16], 34, 99)]);
    let err = env(Ok(none), 1024)
        .set_default("a")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no partition on the boot disk has the U-Boot environment type GUID"),
        "{err}"
    );
    let kind = lemnos_slot_store::gpt::ENV_TYPE_BYTES;
    let two = gpt_disk("two.img", &[(kind, 34, 99), (kind, 100, 227)]);
    let err = env(Ok(two), 1024).set_default("a").unwrap_err().to_string();
    assert!(err.contains("2 partitions on the boot disk have"), "{err}");
    // The partition is there but erased (as flash is), then holds an environment of another size.
    let node = gpt_disk("erased.img", &[(kind, 100, 227)]);
    let mut bytes = fs::read(&node).unwrap();
    bytes[100 * 512..228 * 512].fill(0xFF);
    fs::write(&node, &bytes).unwrap();
    let err = env(Ok(node.clone()), 1024)
        .set_default("a")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("is erased: U-Boot has not saved an environment"),
        "{err}"
    );
    let err = env(Ok(node.clone()), 1024)
        .pending()
        .unwrap_err()
        .to_string();
    assert!(err.contains("is erased"), "{err}");
    // A trial is refused by the environment's state too, before the unknown boot partition is mentioned.
    let err = env(Ok(node.clone()), 1024)
        .set_trial("a", 3)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is erased"), "{err}");
    bytes[100 * 512..228 * 512].fill(0x5A);
    fs::write(&node, &bytes).unwrap();
    let err = env(Ok(node), 8192)
        .set_default("a")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no valid U-Boot environment of 8192 bytes here: U-Boot may be built with another CONFIG_ENV_SIZE"), "{err}");
    // The fixed-offset form: a plain file as the device, the copies where the board file says.
    let plain = dir.0.join("plain.img");
    let place = Place::device(plain.to_str().unwrap(), 1024, 0, 4096).unwrap();
    let mut image = vec![0u8; 8192];
    let g = lemnos_slot_store::uboot_env::Geometry::new(1024, 0, 4096).unwrap();
    let data = lemnos_slot_store::uboot_env::encode_vars(
        &[("default".to_string(), "a".to_string())],
        g.data_len(),
    )
    .unwrap();
    image[..1024].copy_from_slice(&lemnos_slot_store::uboot_env::encode_copy(&data, 3));
    fs::write(&plain, &image).unwrap();
    // The trial names a slot's bay, so the boot partition holds slot `b`.
    let boot = dir.0.join("boot.img");
    fs::write(&boot, crate::bay_fixture::uboot_image("b")).unwrap();
    let mut record = UbootEnv {
        disk: Err("not needed: the device is named".into()),
        place,
        restarter: restarter(),
        boot_node: Ok(boot),
    };
    record.set_trial("b", 3).unwrap();
    assert_eq!(record.pending().unwrap(), Some(("b".to_string(), 3)));
}
