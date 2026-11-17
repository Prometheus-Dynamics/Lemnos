#!/usr/bin/env python3
# The gate's check that packaging/uboot/boot.cmd makes the slot decisions the record calls for, run by
# U-Boot itself: stock U-Boot v2026.10 for the Raspberry Pi 64-bit defconfig (testing/uboot/build-uboot.sh) runs under
# QEMU's raspi3b, with the disk image as the SD card, and runs the boot script, compiled to boot.scr
# with the same release's mkimage, on a disk image that has the boot partition (the slots and their files)
# and the `uboot-env` partition the environment is saved to (found by its GPT type, as U-Boot finds it).
#
# A case is a sequence of boots. Each boot is one U-Boot process that starts from the disk the last boot
# left: the environment is saved by U-Boot's own saveenv, so a boot's writes are what the next boot reads.
# The first `sysboot` the script reaches is the slot the board starts: a started kernel never returns to the
# script, so the boot is ended there (the board's reset). A step that cannot load its tree is reported by
# name and the script goes on. "offering download mode" ends the ladder. Every boot is checked against its
# expectation: which slot starts, the environment after it (read back by a separate U-Boot run), the lines
# the guards print, and that the disk outside the environment is unchanged where a case says so.
#
#   check.py --uboot DIR/u-boot.bin --mkimage DIR/tools/mkimage --work DIR/work [--qemu BIN] [--case NAME] [--script FILE]
#   check.py ... --mutant NAME     (fail-before: the named mutation of boot.cmd must fail its named case)
import argparse
import hashlib
import os
import pathlib
import re
import select
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field

REPO = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_SCRIPT = REPO / "packaging" / "uboot" / "boot.cmd"
UBOOT_ENV_TYPE = "3de21764-95bd-54bd-a5c3-4abe786f38a8"
BOOT_VARS = {
    "devtype": "mmc",
    "devnum": "0",
    "distro_bootpart": "1",
    "scriptaddr": "0x1000000",
    "fdt_addr_r": "0x2000000",
    "pxefile_addr_r": "0x3000000",
}
# The writers' environment for a pending trial and a default (drivers/system-slots: VARIABLES).
WRITERS = {
    "default": "slot-a",
    "default_bay": "bay0",
    "previous_bay": "bay1",
    "trial": "slot-c",
    "trial_bay": "bay2",
    "trial_attempts": "3",
    "lemnos_env": "1",
}
FIRST_SAVE = "lemnos: saved the first U-Boot environment"
NO_PARTITION = "lemnos: no uboot-env partition on mmc 0: the environment is not saved"
RESCUE = "lemnos: rescue after"
DOWNLOAD = "lemnos: no slot started: offering download mode"
STARTED = re.compile(r"^lemnos: (trial|default|previous): (\S+) did not start, going on$")
TIMEOUT = 60


@dataclass
class Boot:
    pick: tuple | None = None            # (step, bay) of the slot that starts; None when none does
    download: bool = False               # the ladder ends in download mode
    env: dict = field(default_factory=dict)   # variables after the boot; "" means unset
    env_same: bool = False               # the boot saves nothing: the environment is unchanged
    lines: list = field(default_factory=list)  # substrings the boot's output must contain
    counts: dict = field(default_factory=dict) # substring -> number of output lines that contain it
    disk_same: bool = False              # the disk's first MiB is unchanged by the boot
    partition_has: list = field(default_factory=list)  # substrings the uboot-env partition holds after the boot


@dataclass
class Case:
    name: str
    about: str
    seed: dict                           # saved to the environment before the first boot
    boots: list
    env_part: bool = True
    ram: dict = field(default_factory=dict)  # set in RAM by every boot; nothing is saved
    no_dtb: tuple = ()                   # bays whose board.dtb is left off the boot partition


def seeded(**changes):
    """The writers' environment with some variables changed; "" removes one."""
    env = dict(WRITERS)
    for key, value in changes.items():
        if value == "":
            env.pop(key, None)
        else:
            env[key] = value
    return env


CASES = [
    Case(
        "normal_trial",
        "a pending trial is started and takes one attempt",
        WRITERS,
        [Boot(pick=("trial", "bay2"),
              env={"trial_attempts": "2", "lemnos_restarts": "1", "trial": "slot-c",
                   "default_bay": "bay0"},
              partition_has=["trial_attempts=2", "lemnos_restarts=1"])],
    ),
    Case(
        "trial_failures_then_previous_at_four_then_rescue",
        "three failed trials use the attempts; at a count of 4 the previous starts first and the trial and "
        "default are left alone; at 8 nothing starts, download mode is offered, and the count stays at 8",
        WRITERS,
        [
            Boot(pick=("trial", "bay2"), env={"trial_attempts": "2", "lemnos_restarts": "1"}),
            Boot(pick=("trial", "bay2"), env={"trial_attempts": "1", "lemnos_restarts": "2"}),
            Boot(pick=("trial", "bay2"), env={"trial_attempts": "0", "lemnos_restarts": "3"}),
            Boot(pick=("previous", "bay1"),
                 env={"trial_attempts": "0", "lemnos_restarts": "4", "trial": "slot-c",
                      "trial_bay": "bay2", "default": "slot-a", "default_bay": "bay0",
                      "previous_bay": "bay1"}),
            Boot(pick=("previous", "bay1"), env={"lemnos_restarts": "5"}),
            Boot(pick=("previous", "bay1"), env={"lemnos_restarts": "6"}),
            Boot(pick=("previous", "bay1"), env={"lemnos_restarts": "7"}),
            Boot(pick=None, download=True, env={"lemnos_restarts": "8"}, lines=[RESCUE]),
            Boot(pick=None, download=True, env_same=True, env={"lemnos_restarts": "8"},
                 lines=[RESCUE]),
        ],
    ),
    Case(
        "no_trial_takes_default",
        "with no attempt left the default starts below the rung",
        seeded(trial_attempts="0"),
        [Boot(pick=("default", "bay0"), env={"lemnos_restarts": "1", "trial_attempts": "0"})],
    ),
    Case(
        "trial_without_files_falls_back_to_default",
        "a trial whose board.dtb cannot load is reported by name; its attempt is still used; the default starts",
        WRITERS,
        [Boot(pick=("default", "bay0"),
              env={"trial_attempts": "2", "lemnos_restarts": "1"},
              lines=["lemnos: trial: cannot load /slots/bay2/board.dtb, going on"])],
        no_dtb=("bay2",),
    ),
    Case(
        "previous_at_four_without_files_default_on_next_pass",
        "at the rung a previous whose files do not load does not take the default in its pass; "
        "the default starts in the next",
        seeded(trial_attempts="", lemnos_restarts="4"),
        [Boot(pick=("default", "bay0"), env={"lemnos_restarts": "5"},
              lines=["lemnos: no slot started (pass x), trying again"])],
        no_dtb=("bay1",),
    ),
    Case(
        "first_environment_saved_once",
        "the first start saves U-Boot's environment with the writers' variables, and never again",
        {k: v for k, v in WRITERS.items() if k != "lemnos_env"},
        [
            Boot(pick=("trial", "bay2"), env={"lemnos_env": "1", "trial_attempts": "2"},
                 counts={FIRST_SAVE: 1}, partition_has=["lemnos_env=1", "trial_attempts=2"]),
            Boot(pick=("trial", "bay2"), env={"trial_attempts": "1", "lemnos_restarts": "2"},
                 counts={FIRST_SAVE: 0}),
        ],
    ),
    Case(
        "no_uboot_env_partition_nothing_is_saved",
        "without the uboot-env partition the trial is not started, nothing is saved, and the disk is not written",
        {},
        [
            Boot(pick=("default", "bay0"), lines=[NO_PARTITION], disk_same=True),
            Boot(pick=("default", "bay0"), lines=[NO_PARTITION], disk_same=True),
        ],
        env_part=False,
        ram=WRITERS,
    ),
    Case(
        "rescue_holds_past_eight_at_9",
        "a count of 9 (past the saturation at 8) still rescues: nothing starts, download mode is offered, and the count "
        "is not raised again",
        seeded(trial_attempts="0", lemnos_restarts="9"),
        [Boot(pick=None, download=True, env_same=True, env={"lemnos_restarts": "9"}, lines=[RESCUE])],
    ),
    Case(
        "rescue_holds_past_eight_at_12",
        "a count of 12 (the bit test of 4 is true again from 12) still rescues: nothing starts, download mode is "
        "offered, and the count is not raised again",
        seeded(trial_attempts="0", lemnos_restarts="12"),
        [Boot(pick=None, download=True, env_same=True, env={"lemnos_restarts": "12"}, lines=[RESCUE])],
    ),
]

# A mutation is one exact replacement in boot.cmd (it must match once). Each names the case it must break.
MUTANTS = {
    # Each mutant is a list of (text in boot.cmd, replacement); every text must occur once in the script. The case named
    # is the one that must fail with the mutant.
    "ladder-rung-at-8": ([
        ('if test "${lemnos_restarts}" -ge 4; then', 'if test "${lemnos_restarts}" -ge 8; then')],
        "trial_failures_then_previous_at_four_then_rescue"),
    "rescue-never": ([
        ('if test "${lemnos_restarts}" -ge 8; then', 'if test "${lemnos_restarts}" -ge 9; then')],
        "trial_failures_then_previous_at_four_then_rescue"),
    # The bit test of 4 (true at 4 to 7, false at 8 to 11, true at 12 to 15) with the exact-8 rescue it came with: the
    # count past 8 bounces between rungs. The rescue and the window are put back as they were.
    "bit-test-restored": ([
        ('\t\tsetenv lemnos_window\n\t\tif test "${lemnos_restarts}" -ge 4; then\n\t\t\tif test "${lemnos_restarts}" -lt 8; then\n'
         '\t\t\t\tsetenv lemnos_window 1\n\t\t\tfi\n\t\tfi\n',
         '\t\tsetenv lemnos_window\n\t\tsetexpr lemnos_bit "${lemnos_restarts}" "&" 4\n'
         '\t\tif test "${lemnos_bit}" = "4"; then\n\t\t\tsetenv lemnos_window 1\n\t\tfi\n'),
        ('\tif test "${lemnos_restarts}" -ge 8; then\n\t\tsetenv lemnos_rescue 1',
         '\tif test "${lemnos_restarts}" = "8"; then\n\t\tsetenv lemnos_rescue 1')],
        "rescue_holds_past_eight_at_9"),
    "trial-saved-without-partition": ([
        ('setenv left\n\t\t\t\tif test -n "${lemnos_saveable}"; then',
         'setenv left\n\t\t\t\tif test -n "1"; then')],
        "no_uboot_env_partition_nothing_is_saved"),
    "first-save-every-boot": ([
        ('if test -z "${lemnos_env}"; then', 'if test -n "x"; then')],
        "first_environment_saved_once"),
    "attempts-off-by-one": ([
        ('setenv left 2', 'setenv left 1')],
        "normal_trial"),
    "default-never-named": ([
        ('setenv take_default ${default}', 'setenv take_default')],
        "no_trial_takes_default"),
    "previous-before-default": ([
        ('for step in trial default previous; do', 'for step in trial previous default; do')],
        "trial_without_files_falls_back_to_default"),
    "rung-never-skips-default": ([
        ('if test -z "${lemnos_pass}"; then', 'if test -n "x"; then')],
        "previous_at_four_without_files_default_on_next_pass"),
}


def write_gpt(path, size_bytes, parts):
    """Writes a GPT disk: a protective MBR, the primary header and entries, and the backup. parts is a list of
    (first_lba, last_lba, type_guid, name). Written by hand because sgdisk 1.0.10 takes 4-digit type codes only and
    waited (in nanosleep, under the load of this PC) for longer than the bound on this disk."""
    import struct
    import uuid
    import zlib
    sector = 512
    total = size_bytes // sector
    entries = bytearray(128 * 128)
    for i, (first, last, type_guid, name) in enumerate(parts):
        e = bytearray(128)
        e[0:16] = uuid.UUID(type_guid).bytes_le
        e[16:32] = uuid.uuid4().bytes_le
        struct.pack_into("<QQ", e, 32, first, last)
        e[56:56 + 72] = name.encode("utf-16-le")[:72].ljust(72, b"\0")
        entries[i * 128:(i + 1) * 128] = e
    entries_crc = zlib.crc32(bytes(entries)) & 0xFFFFFFFF
    entry_sectors = len(entries) // sector  # 32
    first_usable = 2 + entry_sectors
    last_usable = total - 1 - entry_sectors - 1
    disk_guid = uuid.uuid4().bytes_le

    def header(my, alt, entries_lba):
        h = bytearray(sector)
        h[0:8] = b"EFI PART"
        struct.pack_into("<IIIIQQQQ", h, 8, 0x00010000, 92, 0, 0, my, alt, first_usable, last_usable)
        h[56:72] = disk_guid
        struct.pack_into("<QIII", h, 72, entries_lba, 128, 128, entries_crc)
        struct.pack_into("<I", h, 16, zlib.crc32(bytes(h[:92])) & 0xFFFFFFFF)
        return h

    mbr = bytearray(sector)
    mbr[446:462] = bytes([0x00, 0x00, 0x02, 0x00, 0xEE, 0xFF, 0xFF, 0xFF]) + struct.pack("<II", 1, total - 1)[:8]
    mbr[510:512] = b"\x55\xaa"
    primary = header(1, total - 1, 2)
    backup = header(total - 1, 1, total - 1 - entry_sectors)
    with open(path, "r+b") as f:
        f.seek(0); f.write(mbr)
        f.seek(sector); f.write(primary)
        f.seek(2 * sector); f.write(entries)
        f.seek((total - 1 - entry_sectors) * sector); f.write(entries)
        f.seek((total - 1) * sector); f.write(backup)


class Failure(Exception):
    pass


def _wait_state(pid):
    """What the kernel says a stuck child is waiting on: wchan, state and (where readable) the kernel stack.
    Captured before the bound kills the child, so a hang can be read afterwards."""
    out = []
    for name in ("wchan", "status", "stack"):
        try:
            with open(f"/proc/{pid}/{name}", "rb") as f:
                text = f.read().decode(errors="replace").strip()
        except OSError as e:
            text = f"unreadable ({e.strerror})"
        if name == "status":
            text = " ".join(l for l in text.splitlines() if l.startswith(("State:", "Name:")))
        out.append(f"{name}: {text}")
    return "; ".join(out)


class Console:
    """One QEMU run with its serial console on a pipe. Output is read in bounded steps; the process is killed
    (ours) when the case is done with it, which is the board's reset."""

    def __init__(self, args, cwd):
        self.proc = subprocess.Popen(args, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, bufsize=0)
        self.fd = self.proc.stdout.fileno()
        self.buf = b""

    def fill(self, deadline, what):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise Failure(f"no output for {what} within {TIMEOUT} s; process state: {_wait_state(self.proc.pid)}")
        ready, _, _ = select.select([self.fd], [], [], min(0.2, remaining))
        if ready:
            chunk = os.read(self.fd, 65536)
            if not chunk:
                raise Failure(f"the U-Boot console ended while waiting for {what}")
            self.buf += chunk
        return True

    def until(self, pattern, what, timeout=TIMEOUT):
        """Reads up to and including `pattern`; returns the text read."""
        deadline = time.monotonic() + timeout
        while True:
            i = self.buf.find(pattern)
            if i >= 0:
                text = self.buf[:i + len(pattern)]
                self.buf = self.buf[i + len(pattern):]
                return text.decode(errors="replace").replace("\r", "")
            self.fill(deadline, what)

    def send(self, line):
        self.proc.stdin.write(line.encode() + b"\n")
        self.proc.stdin.flush()

    def lines(self, stop, timeout=TIMEOUT):
        """Yields complete console lines until stop(line) is true or the timeout passes."""
        deadline = time.monotonic() + timeout
        while True:
            while b"\n" in self.buf:
                raw, self.buf = self.buf.split(b"\n", 1)
                line = raw.decode(errors="replace").rstrip("\r")
                yield line
                if stop(line):
                    return
            self.fill(deadline, "a line of the boot script")

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait()
        if self.proc.stdout:
            self.proc.stdout.close()


class Harness:
    def __init__(self, uboot, mkimage, work, script, qemu, dtb):
        self.dtb = dtb            # the device tree the firmware hands U-Boot (bcm2837-rpi-3-b)
        self.uboot = uboot        # the bootloader image (u-boot.bin)
        self.mkimage = mkimage
        self.work = work
        self.script = script
        self.qemu = qemu
        self.scr = None

    def run(self, args, cwd, timeout=TIMEOUT):
        """A bounded external command. A command that does not finish in `timeout` s is not waited on: its state is
        read from /proc and it is killed, and the case fails with that state in the message."""
        proc = subprocess.Popen(args, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE)
        try:
            out, err = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            state = _wait_state(proc.pid)
            proc.kill()
            proc.wait()
            raise Failure(f"{args[0]} did not finish in {timeout} s (bounded; killed): {state}")
        return subprocess.CompletedProcess(args, proc.returncode, out, err)

    def build_tools(self):
        """boot.scr, compiled from the script with the release's own mkimage."""
        self.work.mkdir(parents=True, exist_ok=True)
        scr = self.work / "boot.scr"
        r = self.run([str(self.mkimage), "-A", "arm64", "-O", "linux", "-T", "script", "-C", "none",
                      "-n", "lemnos boot.cmd", "-d", str(self.script), str(scr)], self.work)
        if r.returncode:
            raise Failure("mkimage failed: " + r.stderr.decode(errors="replace"))
        self.scr = scr

    def disk(self, case, root):
        """A GPT disk: partition 1 is the boot FAT with the slots; partition 2 is the uboot-env partition."""
        img = root / "disk.img"
        with open(img, "wb") as f:
            f.truncate(32 * 1024 * 1024)
        parts = [(2048, 18431, "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7", "boot")]
        if case.env_part:
            # the uboot-env partition: type = the U-Boot environment GUID (env/mmc.c finds it by this type)
            parts.append((18432, 20479, UBOOT_ENV_TYPE, "uboot-env"))
        write_gpt(img, 32 * 1024 * 1024, parts)
        fat = root / "boot.img"
        r = self.run(["mkfs.fat", "-C", str(fat), "8192"], root)
        if r.returncode:
            raise Failure("mkfs.fat: " + r.stderr.decode(errors="replace").strip())
        dtb = root / "board.dtb"
        dts = root / "board.dts"
        dts.write_text("/dts-v1/;\n/ { compatible = \"lemnos,case\"; chosen { }; };\n")
        r = self.run(["dtc", "-I", "dts", "-O", "dtb", "-o", str(dtb), str(dts)], root)
        if r.returncode:
            raise Failure("dtc: " + r.stderr.decode(errors="replace").strip())
        def mtool(args):
            # mtools makes one directory level per call; a failed call is a failure of the case, not a skip.
            r = self.run(args, root)
            if r.returncode:
                raise Failure(f"{args[0]} {' '.join(args[2:])}: " + r.stderr.decode(errors="replace").strip())

        mtool(["mmd", "-i", str(fat), "::/slots"])
        for bay in ("bay0", "bay1", "bay2"):
            mtool(["mmd", "-i", str(fat), f"::/slots/{bay}"])
            mtool(["mmd", "-i", str(fat), f"::/slots/{bay}/extlinux"])
            if bay not in case.no_dtb:
                mtool(["mcopy", "-i", str(fat), "-D", "o", str(dtb), f"::/slots/{bay}/board.dtb"])
            conf = root / "extlinux.conf"
            conf.write_text(f"LABEL slot\n\tKERNEL /slots/{bay}/Image\n")
            mtool(["mcopy", "-i", str(fat), "-D", "o", str(conf), f"::/slots/{bay}/extlinux/extlinux.conf"])
        mtool(["mcopy", "-i", str(fat), "-D", "o", str(self.scr), "::/boot.scr"])
        with open(img, "r+b") as disk, open(fat, "rb") as part:
            disk.seek(2048 * 512)
            disk.write(part.read())
        return root

    def qemu_args(self, root):
        # raspi3b: the firmware's device tree is passed with -dtb (U-Boot's OF_BOARD takes it from x0), the mini UART
        # (the second serial) is the console, and the disk is the SD card.
        return [str(self.qemu), "-M", "raspi3b", "-m", "1024", "-display", "none",
                "-serial", "null", "-serial", "stdio", "-dtb", str(self.dtb),
                "-kernel", str(self.uboot), "-drive", "if=sd,format=raw,file=" + str(root / "disk.img")]

    def session(self, root):
        """A U-Boot run on the case's disk: stops the autoboot countdown, and returns at the prompt."""
        c = Console(self.qemu_args(root), root)
        c.until(b"Hit any key", "the autoboot prompt")
        c.send("")
        c.until(b"U-Boot> ", "the U-Boot prompt")
        return c

    def seed(self, root, env):
        """Saves the given variables (with the boot variables) to the environment, as the writers do."""
        assignments = {**BOOT_VARS, **env}
        cmd = "; ".join(f"setenv {k} {v}" for k, v in assignments.items()) + "; saveenv"
        c = self.session(root)
        try:
            c.send(cmd)
            text = c.until(b"U-Boot> ", "saveenv")
            if "Saving Environment to MMC... " not in text or "OK" not in text.split("Saving Environment to MMC... ", 1)[1].split("\n", 1)[0]:
                raise Failure("seeding the environment did not save to MMC: " + text[-300:])
        finally:
            c.close()

    def env(self, root):
        c = self.session(root)
        try:
            c.send("printenv")
            text = c.until(b"U-Boot> ", "printenv")
        finally:
            c.close()
        values = {}
        for line in text.splitlines():
            m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", line.strip())
            if m:
                values[m.group(1)] = m.group(2)
        return values

    def boot(self, root, ram):
        """One boot: the boot flow's load and source, ended at the first sysboot or the download-mode offer."""
        preset = "".join(f"setenv {k} {v}; " for k, v in ram.items())
        # The working device tree is the control tree, as on a board: U-Boot sets fdtcontroladdr to the control tree,
        # and the script reads /aliases from the working tree (fdt addr -c only prints it).
        command = preset + "fdt addr ${fdtcontroladdr}; load mmc 0:1 ${scriptaddr} boot.scr; source ${scriptaddr}"
        c = self.session(root)
        lines, pick, result = [], None, None
        try:
            c.send(command)

            def stop(line):
                nonlocal pick, result
                lines.append(line)
                m = STARTED.match(line)
                if m:
                    pick = (m.group(1), m.group(2).split("/")[2])
                    result = "started"
                    return True
                if line.startswith(DOWNLOAD):
                    result = "download"
                    return True
                return False

            for _ in c.lines(stop):
                pass
            if result is None:
                result = "exited"
        finally:
            c.close()
        return result, pick, lines

    def disk_head(self, root):
        return hashlib.sha256((root / "disk.img").read_bytes()[: 1024 * 1024]).hexdigest()


def env_partition_bytes(root):
    """The bytes of the uboot-env partition (partition 2 of the disk image, both environment copies)."""
    start = 18432 * 512  # partition 2 starts at sector 18432 (boot partition: 2048 + 16384 sectors)
    return (root / "disk.img").read_bytes()[start:start + 1024 * 1024]


def check_case(h, case, root, report):
    root.mkdir(parents=True, exist_ok=True)
    h.disk(case, root)
    if case.seed:
        h.seed(root, case.seed)
    problems = []
    for number, boot in enumerate(case.boots, 1):
        before_env = h.env(root) if case.env_part else {}
        before_disk = h.disk_head(root)
        ram = {**BOOT_VARS, **case.ram} if not case.env_part else dict(case.ram)
        result, pick, lines = h.boot(root, ram)
        report.append(f"    boot {number}: result={result} pick={pick}")
        if boot.download:
            if result != "download":
                problems.append(f"boot {number}: expected download mode, got {result} ({pick})")
        elif boot.pick is None:
            problems.append(f"boot {number}: expected no slot to start, got {result} {pick}")
        elif pick != boot.pick or result != "started":
            problems.append(f"boot {number}: expected {boot.pick}, got {pick} ({result})")
        for want in boot.lines:
            if not any(want in line for line in lines):
                problems.append(f"boot {number}: no line containing {want!r}")
        for want, count in boot.counts.items():
            got = sum(want in line for line in lines)
            if got != count:
                problems.append(f"boot {number}: {want!r} printed {got} times, expected {count}")
        if case.env_part:
            after_env = h.env(root)
            for key, value in boot.env.items():
                got = after_env.get(key, "")
                if got != value:
                    problems.append(f"boot {number}: {key} is {got!r} after the boot, expected {value!r}")
            if boot.env_same and after_env != before_env:
                changed = sorted(k for k in set(before_env) | set(after_env)
                                 if before_env.get(k) != after_env.get(k))
                problems.append(f"boot {number}: the environment changed: {changed}")
        if boot.partition_has:
            part = env_partition_bytes(root)
            for want in boot.partition_has:
                if want.encode() not in part:
                    problems.append(f"boot {number}: {want!r} is not in the uboot-env partition after the boot")
        if boot.disk_same and h.disk_head(root) != before_disk:
            problems.append(f"boot {number}: the disk outside the environment was written")
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--uboot", required=True, type=pathlib.Path)
    ap.add_argument("--mkimage", required=True, type=pathlib.Path)
    ap.add_argument("--qemu", default="qemu-system-aarch64", type=pathlib.Path)
    ap.add_argument("--dtb", required=True, type=pathlib.Path)
    ap.add_argument("--work", required=True, type=pathlib.Path)
    ap.add_argument("--script", type=pathlib.Path, default=DEFAULT_SCRIPT)
    ap.add_argument("--case", action="append", default=[])
    ap.add_argument("--mutant", choices=sorted(MUTANTS))
    args = ap.parse_args(argv)

    script = args.script
    if args.mutant:
        pairs, _ = MUTANTS[args.mutant]
        text = script.read_text()
        for old, new in pairs:
            if text.count(old) != 1:
                raise SystemExit(f"mutant {args.mutant}: anchor found {text.count(old)} times, need 1: {old[:60]!r}")
            text = text.replace(old, new)
        args.work.mkdir(parents=True, exist_ok=True)
        script = args.work / f"boot.{args.mutant}.cmd"
        script.write_text(text)

    cases = [c for c in CASES if not args.case or c.name in args.case]
    h = Harness(args.uboot.resolve(), args.mkimage.resolve(), args.work.resolve(), script.resolve(),
                shutil.which(str(args.qemu)) or str(args.qemu), args.dtb.resolve())
    h.build_tools()
    failed = {}
    for case in cases:
        root = (args.work / case.name).resolve()
        if root.exists():
            shutil.rmtree(root)
        report = []
        try:
            problems = check_case(h, case, root, report)
        except Failure as e:
            problems = [str(e)]
        status = "ok" if not problems else "FAIL"
        print(f"{status}: {case.name}: {case.about}")
        if problems:
            failed[case.name] = problems
            print("\n".join(report))
            for p in problems:
                print(f"    {p}")
    if args.mutant:
        want = MUTANTS[args.mutant][1]
        if want in failed:
            print(f"mutant {args.mutant} caught by {want}")
            return 0
        print(f"mutant {args.mutant} was NOT caught by {want}")
        return 1
    if failed:
        print(f"uboot check: {len(failed)} of {len(cases)} cases failed")
        return 1
    print(f"uboot check: {len(cases)} cases passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
