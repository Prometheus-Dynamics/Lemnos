#!/usr/bin/env python3
"""Tests for scripts/check-unit-grants.py (run by scripts/gate.sh).

Each case builds a small tree in a temporary directory (a daemon crate that links one driver crate, a unit,
a drop-in, udev rules, files.txt and the grants table), changes one thing, and runs the checker on it with
`--root`. A case that should fail must fail with the named message; a case that should hold must exit 0.
The real tree is checked too. Nothing is left behind: each tree is removed when its case ends.
"""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

# No bytecode for the checker's module, here or in a child: a test leaves nothing in scripts/.
sys.dont_write_bytecode = True

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
CHECK = os.path.join(ROOT, "scripts", "check-unit-grants.py")
NAMESPACE = os.path.join(ROOT, "testing", "crates", "unit-check", "src", "namespace.rs")


def load_checker():
    spec = importlib.util.spec_from_file_location("check_unit_grants", CHECK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

UDEV_LINE = 'SUBSYSTEM=="misc", KERNEL=="foo0", GROUP="lemnos", MODE="0660"'

BASE = {
    "Cargo.toml": "[workspace]\nmembers = []\n\n[workspace.dependencies]\n",
    "service/lemnosd/Cargo.toml": '[package]\nname = "lemnosd"\nversion = "0.0.0"\n\n[dependencies]\nfoo = { path = "../../drivers/foo" }\n',
    "service/lemnosd/src/lib.rs": 'pub fn run() {\n    let _ = "/run/lemnos/lemnosd.sock";\n    let _ = "CAP_SYS_RAWIO";\n}\n',
    "drivers/foo/Cargo.toml": '[package]\nname = "foo"\nversion = "0.0.0"\n',
    "drivers/foo/src/lib.rs": 'pub fn open() {\n    let _ = "/dev/foo0";\n}\n',
    "packaging/systemd/lemnosd.service": "[Service]\nRuntimeDirectory=lemnos\nCapabilityBoundingSet=CAP_SYS_RAWIO\nProtectKernelTunables=yes\n",
    "service/lemnosd/src/privilege.rs": "fn steps() {\n    let _ = lemnos_driver_foo::grant_before_drop();\n}\n",
    "packaging/udev/70-lemnos.rules": UDEV_LINE + "\n",
    "packaging/files.txt": "# header\n",
    "packaging/init/mdev.conf.fragment": "# header\n^foo0$                  root:lemnos 0660\n",
    "service/devtree/tests/golden/late-grants-udev.rules": "# header\n",
    "service/devtree/tests/golden/late-grants-mdev.conf": "# header\n",
    "service/devtree/tests/golden/late-grants-chip-udev.rules": "# header\n",
    "service/devtree/tests/golden/late-grants-chip-mdev.conf": "# header\n",
    "packaging/systemd/grants.toml": """grant = [
  {path = "/run/lemnos", kind = "path", access = "rw", how = "unit", via = "packaging/systemd/lemnosd.service", token = "RuntimeDirectory:lemnos", drivers = ["service/lemnosd"], reason = "the socket directory"},
  {path = "CAP_SYS_RAWIO", kind = "cap", access = "rw", how = "unit", via = "packaging/systemd/lemnosd.service", token = "CapabilityBoundingSet:CAP_SYS_RAWIO", drivers = ["service/lemnosd"], needle = "CAP_SYS_RAWIO", reason = "the kept capability"},
  {path = "/dev/foo0", kind = "device", access = "rw", how = "udev", via = "packaging/udev/70-lemnos.rules", token = 'SUBSYSTEM=="misc", KERNEL=="foo0", GROUP="lemnos", MODE="0660"', subsystem = "misc", node = "foo0", mdev = "^foo0$", drivers = ["drivers/foo"], reason = "the foo node"},
]
""",
}


class Tree:
    """A temporary root with files written from a dict; changes are made with `put`."""

    def __init__(self, files):
        self.dir = tempfile.mkdtemp(prefix="unit-grants-")
        for path, text in files.items():
            self.put(path, text)

    def put(self, path, text):
        full = os.path.join(self.dir, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w", encoding="utf-8") as f:
            f.write(text)

    def check(self):
        return subprocess.run([sys.executable, "-I", CHECK, "--root", self.dir],
                              capture_output=True, text=True)

    def close(self):
        shutil.rmtree(self.dir, ignore_errors=True)


class UnitGrantsTest(unittest.TestCase):
    def tree(self, **changes):
        files = dict(BASE)
        files.update(changes)
        tree = Tree(files)
        self.addCleanup(tree.close)
        return tree

    def assertHolds(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("holds", result.stdout)

    def assertFails(self, result, *fragments):
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        for fragment in fragments:
            self.assertIn(fragment, result.stderr)

    def test_the_small_tree_holds(self):
        self.assertHolds(self.tree().check())

    def test_a_driver_path_nothing_lists_fails(self):
        tree = self.tree()
        tree.put("drivers/foo/src/lib.rs", 'pub fn open() {\n    let _ = "/dev/foo0";\n    let _ = "/dev/bar0";\n}\n')
        self.assertFails(tree.check(), "drivers/foo/src/lib.rs:3", "/dev/bar0", "which no row")

    def test_a_path_in_a_test_block_or_test_file_is_not_needed(self):
        tree = self.tree()
        tree.put("drivers/foo/src/lib.rs",
                 'pub fn open() {\n    let _ = "/dev/foo0";\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let _ = "/dev/bar0";\n    }\n}\n')
        tree.put("drivers/foo/src/tests.rs", 'fn t() {\n    let _ = "/dev/baz0";\n}\n')
        self.assertHolds(tree.check())

    def test_a_udev_group_grant_nothing_lists_fails(self):
        tree = self.tree()
        tree.put("packaging/udev/70-lemnos.rules", UDEV_LINE + "\n" +
                 'SUBSYSTEM=="misc", KERNEL=="baz0", GROUP="lemnos", MODE="0660"\n')
        self.assertFails(tree.check(), "70-lemnos.rules", "declares")

    def test_a_busybox_line_nothing_declares_fails(self):
        tree = self.tree()
        tree.put("packaging/init/mdev.conf.fragment", "^foo0$                  root:lemnos 0660\n^baz0$  root:lemnos 0660\n")
        self.assertFails(tree.check(), "mdev.conf.fragment", "^baz0$")

    def test_a_udev_row_without_its_busybox_line_fails(self):
        tree = self.tree()
        tree.put("packaging/init/mdev.conf.fragment", "# header\n")
        self.assertFails(tree.check(), "no mdev line")

    def test_a_row_nothing_needs_fails(self):
        tree = self.tree()
        tree.put("packaging/udev/70-lemnos.rules", UDEV_LINE + "\n" +
                 'SUBSYSTEM=="misc", KERNEL=="qux0", GROUP="lemnos", MODE="0660"\n')
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/dev/qux0", kind = "device", access = "rw", how = "udev", via = "packaging/udev/70-lemnos.rules", '
            'token = \'SUBSYSTEM=="misc", KERNEL=="qux0", GROUP="lemnos", MODE="0660"\', subsystem = "misc", node = "qux0", '
            'drivers = ["drivers/foo"], reason = "nobody"},\n]\n'))
        self.assertFails(tree.check(), "/dev/qux0", "needed by none")

    def test_a_row_naming_a_grant_the_files_do_not_make_fails(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            'RuntimeDirectory:lemnos', 'RuntimeDirectory:other'))
        self.assertFails(tree.check(), "the packaged files do not make")

    def test_a_row_for_a_node_its_rule_does_not_match_fails(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            'node = "foo0"', 'node = "bar0"'))
        self.assertFails(tree.check(), "not matched by the rule's KERNEL")

    def test_a_write_row_on_a_path_the_unit_leaves_read_only_fails(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/etc/foo", kind = "path", access = "rw", how = "none", drivers = ["drivers/foo"], reason = "x"},\n]\n'))
        self.assertFails(tree.check(), "/etc/foo")

    def test_a_drop_in_under_the_wrong_board_condition_fails(self):
        tree = self.tree()
        tree.put("packaging/files.txt", "packaging/systemd/lemnosd.service.d/security.conf /etc/x 0644 security\n")
        tree.put("packaging/systemd/lemnosd.service.d/security.conf", "[Service]\nReadWritePaths=/run/board\n")
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/run/board", kind = "path", access = "rw", how = "drop-in", via = "packaging/systemd/lemnosd.service.d/security.conf", '
            'token = "ReadWritePaths:/run/board", when = "slots", drivers = ["drivers/foo"], reason = "x"},\n]\n'))
        self.assertFails(tree.check(), "not installed under `slots`")

    def test_a_dynamic_path_with_no_open_tag_fails(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/sys/class/foo", kind = "path", access = "rw", how = "dynamic", site = "drivers/foo/src/lib.rs", needle = "pub fn open", drivers = ["drivers/foo"], reason = "x"},\n]\n'))
        self.assertFails(tree.check(), "tag it `open`")

    def test_a_dynamic_path_tagged_open_holds_and_is_listed(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/sys/class/foo", kind = "path", access = "rw", how = "dynamic", open = "PWM-SYSFS-UNIT", site = "drivers/foo/src/lib.rs", needle = "pub fn open", drivers = ["drivers/foo"], reason = "x"},\n]\n'))
        result = tree.check()
        self.assertHolds(result)
        self.assertIn("open (PWM-SYSFS-UNIT): /sys/class/foo", result.stdout)

    def test_a_dynamic_site_that_no_longer_builds_the_path_is_stale(self):
        tree = self.tree()
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace(
            "]\n",
            '  {path = "/sys/class/foo", kind = "path", access = "rw", how = "dynamic", open = "X", site = "drivers/foo/src/lib.rs", needle = "pub fn gone", drivers = ["drivers/foo"], reason = "x"},\n]\n'))
        self.assertFails(tree.check(), "no longer holds 'pub fn gone'")

    def add_row(self, tree, row):
        tree.put("packaging/systemd/grants.toml", BASE["packaging/systemd/grants.toml"].replace("]\n", row + "\n]\n"))

    def test_a_root_phase_row_whose_call_is_not_wired_fails(self):
        tree = self.tree()
        tree.put("service/lemnosd/src/privilege.rs", "fn steps() {}\n")
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", wired = "grant_before_drop", drivers = ["drivers/foo"], reason = "x"},')
        self.assertFails(tree.check(), "does not call `grant_before_drop`", "the grant is never made")

    def test_a_renamed_wired_call_is_not_the_call_it_names(self):
        tree = self.tree()
        tree.put("service/lemnosd/src/privilege.rs", "fn steps() {\n    let _ = lemnos_driver_foo::grant_before_drop_removed();\n}\n")
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", wired = "grant_before_drop", drivers = ["drivers/foo"], reason = "x"},')
        self.assertFails(tree.check(), "does not call `grant_before_drop`")

    def test_a_comment_that_names_the_wired_call_does_not_wire_it(self):
        tree = self.tree()
        tree.put("service/lemnosd/src/privilege.rs", "/// The step calls `lemnos_driver_foo::grant_before_drop`.\nfn steps() {}\n")
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", wired = "lemnos_driver_foo::grant_before_drop", drivers = ["drivers/foo"], reason = "x"},')
        self.assertFails(tree.check(), "does not call `lemnos_driver_foo::grant_before_drop`")

    def test_a_root_phase_row_with_no_wired_call_fails(self):
        tree = self.tree()
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", drivers = ["drivers/foo"], reason = "x"},')
        self.assertFails(tree.check(), "names the call that makes its grant")

    def test_a_root_phase_site_that_no_longer_holds_its_needle_fails(self):
        tree = self.tree()
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn gone", wired = "grant_before_drop", drivers = ["drivers/foo"], reason = "x"},')
        self.assertFails(tree.check(), "no longer holds 'pub fn gone'")

    def test_a_root_phase_row_under_sys_holds_with_the_tunables_protected(self):
        # PWM-SYSFS-UNIT: /sys stays writable under the base unit (ProtectKernelTunables=yes), so the chown's
        # target needs no ReadWritePaths.
        tree = self.tree()
        self.add_row(tree, '  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", wired = "grant_before_drop", drivers = ["drivers/foo"], reason = "x"},')
        self.assertHolds(tree.check())

    def reach_row(self, tree, path):
        """A root-phase grant on `path` (it reaches the reach check; its site and wiring hold)."""
        self.add_row(tree, '  {path = "%s", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", needle = "pub fn open", wired = "grant_before_drop", drivers = ["drivers/foo"], reason = "x"},' % path)

    def test_a_write_row_under_the_tunables_table_fails(self):
        tree = self.tree()
        self.reach_row(tree, "/sys/kernel/debug/foo")
        self.assertFails(tree.check(), "/sys/kernel/debug/foo is rw, and the unit leaves it read-only")

    def test_a_write_row_under_the_tunables_table_holds_without_the_protection(self):
        tree = self.tree()
        tree.put("packaging/systemd/lemnosd.service", "[Service]\nRuntimeDirectory=lemnos\nCapabilityBoundingSet=CAP_SYS_RAWIO\n")
        self.reach_row(tree, "/sys/kernel/debug/foo")
        self.assertHolds(tree.check())

    def test_a_drop_in_that_unprotects_tunables_makes_the_tunables_table_writable(self):
        # systemd's last assignment wins: the drop-in's `no` is the unit's setting.
        tree = self.tree()
        tree.put("packaging/systemd/lemnosd.service.d/x.conf", "[Service]\nProtectKernelTunables=no\n")
        tree.put("packaging/files.txt", "packaging/systemd/lemnosd.service.d/x.conf /etc/x 0644 never\n")
        self.reach_row(tree, "/sys/kernel/debug/foo")
        self.assertHolds(tree.check())

    def test_a_cgroup_write_row_fails_with_control_groups_protected(self):
        tree = self.tree()
        tree.put("packaging/systemd/lemnosd.service", "[Service]\nRuntimeDirectory=lemnos\nCapabilityBoundingSet=CAP_SYS_RAWIO\nProtectControlGroups=yes\n")
        self.reach_row(tree, "/sys/fs/cgroup/foo")
        self.assertFails(tree.check(), "/sys/fs/cgroup/foo is rw, and the unit leaves it read-only")

    def test_the_tunables_list_is_unit_checks(self):
        # The checker's list is namespace.rs's TUNABLES, the same entries in the same order.
        with open(NAMESPACE, encoding="utf-8") as f:
            text = f.read()
        block = re.search(r"pub const TUNABLES: &\[&str\] = &\[(.*?)\];", text, re.S)
        self.assertIsNotNone(block, "namespace.rs has no TUNABLES list")
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', block.group(1))), load_checker().TUNABLES)

    def test_the_writable_rule_is_unit_checks_writable(self):
        # The cases of namespace.rs's own tests for `writable` (with the tunables and control groups protected).
        checker = load_checker()
        on = {"ProtectKernelTunables": "yes", "ProtectControlGroups": "yes"}
        self.assertTrue(checker.base_writable("/proc", on))
        self.assertFalse(checker.base_writable("/proc/sys", on))
        self.assertTrue(checker.base_writable("/sys", on))
        self.assertTrue(checker.base_writable("/sys/kernel/config", on))
        self.assertTrue(checker.base_writable("/sys/devices/platform/x/power/control", on))
        self.assertFalse(checker.base_writable("/sys/kernel/debug", on))
        self.assertFalse(checker.base_writable("/sys/kernel/tracing", on))
        self.assertFalse(checker.base_writable("/sys/fs/cgroup", on))
        self.assertFalse(checker.base_writable("/etc", on))
        self.assertTrue(checker.base_writable("/run/lemnos", on))

    HOTPLUG_UDEV = 'SUBSYSTEM=="hwmon", ACTION=="add", ATTR{name}=="pwmfan", TEST=="pwm1", RUN+="/bin/chgrp lemnos $sys$devpath/pwm1", RUN+="/bin/chmod 0660 $sys$devpath/pwm1"'
    HOTPLUG_MDEV = "-SUBSYSTEM=^hwmon$;^hwmon[0-9]+$ root:root 0660 @/bin/sh -c 'for a in pwm1; do /bin/chgrp lemnos /sys$DEVPATH/$a && /bin/chmod 0660 /sys$DEVPATH/$a; done'"
    WIRED_PRIVILEGE = ("fn steps() {\n    let _ = lemnos_driver_foo::grant_before_drop();\n"
                       "    let _ = linux.grant_pwm_before_drop(x);\n"
                       "    let _ = lemnos_driver_hwmon_fan::grant_before_drop(c);\n}\n")

    def hotplug_row(self, tree, udev=(), mdev=(), wired="grant_before_drop"):
        """A root-phase grant that names its hotplug lines (as the table's late grants do)."""
        row = ('  {path = "/sys/devices/foo", kind = "path", access = "rw", how = "root-phase", site = "drivers/foo/src/lib.rs", '
               'needle = "pub fn open", wired = "%s", drivers = ["drivers/foo"], hotplug_udev = [%s], hotplug_mdev = [%s], reason = "x"},'
               % (wired, ", ".join(json.dumps(line) for line in udev), ", ".join(json.dumps(line) for line in mdev)))
        self.add_row(tree, row)

    def generated_late_grants(self, tree, udev=None, mdev=None):
        """The generator's golden output for a board with the late grant (the lines the rows may name)."""
        tree.put("service/devtree/tests/golden/late-grants-udev.rules",
                 "# header\n" + (self.HOTPLUG_UDEV if udev is None else udev) + "\n")
        tree.put("service/devtree/tests/golden/late-grants-mdev.conf",
                 "# header\n" + (self.HOTPLUG_MDEV if mdev is None else mdev) + "\n")

    def test_a_late_grant_named_on_both_golden_files_holds(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertHolds(tree.check())

    def test_a_named_udev_line_the_generator_does_not_make_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV.replace('TEST=="pwm1"', 'TEST=="pwm2"')], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "its hotplug line", "is not in service/devtree/tests/golden/late-grants-udev.rules")

    def test_a_named_mdev_line_the_generator_does_not_make_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV.replace("pwm1;", "pwm2;")], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "is not in service/devtree/tests/golden/late-grants-mdev.conf")

    def test_a_late_grant_the_packaged_rules_file_holds_fails(self):
        # The board's late grants are written by the generator, never by the package: a fixed hwmon or PWM grant
        # in packaging/udev/70-lemnos.rules is refused by name.
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("packaging/udev/70-lemnos.rules", UDEV_LINE + "\n" + self.HOTPLUG_UDEV + "\n")
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "packaging/udev/70-lemnos.rules", "the board's generator writes it")

    def test_a_late_grant_the_busybox_fragment_holds_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("packaging/init/mdev.conf.fragment", "# header\n^foo0$                  root:lemnos 0660\n" + self.HOTPLUG_MDEV + "\n")
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "packaging/init/mdev.conf.fragment", "the board's generator writes it")

    def test_a_generated_late_grant_with_no_row_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree, udev=self.HOTPLUG_UDEV + "\n" + self.HOTPLUG_UDEV.replace('pwmfan', 'otherfan'))
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "otherfan", "is a late grant and no row declares it")

    def test_a_generated_mdev_late_grant_with_no_row_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree, mdev=self.HOTPLUG_MDEV + "\n" + self.HOTPLUG_MDEV.replace("^hwmon[0-9]+$", "^hwmon9$"))
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [self.HOTPLUG_MDEV], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "hwmon9", "is a late grant and no row declares it")

    def test_a_pwm_or_fan_root_phase_grant_without_its_hotplug_lines_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [], [], wired="grant_pwm_before_drop")
        self.assertFails(tree.check(), "names its hotplug lines")

    def test_a_late_grant_with_only_one_of_its_two_files_named_fails(self):
        tree = self.tree()
        self.generated_late_grants(tree)
        tree.put("service/lemnosd/src/privilege.rs", self.WIRED_PRIVILEGE)
        self.hotplug_row(tree, [self.HOTPLUG_UDEV], [], wired="lemnos_driver_hwmon_fan::grant_before_drop")
        self.assertFails(tree.check(), "names its hotplug lines")

    def test_the_real_tree_holds(self):
        result = subprocess.run([sys.executable, "-I", CHECK], capture_output=True, text=True, cwd=ROOT)
        self.assertHolds(result)


if __name__ == "__main__":
    unittest.main()
