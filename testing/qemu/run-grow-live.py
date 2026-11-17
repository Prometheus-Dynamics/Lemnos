#!/usr/bin/env python3
"""The GROW2 liveness and power-cut check: the engine host (lemnos-grow-host) with system-slots on the real
disk and hw-watchdog on a QEMU i6300esb, in an x86 guest, with a 2 TiB resize.

  run-grow-live.py <mode> --kernel BZIMAGE --harness LEMNOS_GROW_HOST --init GROW_INIT --work DIR [--size B] [--kill-at S]

Modes:
  wd3        watchdog timeout 3 s (fed at half): the guest must not be reset; the max feed and call gaps are reported
  wd15       watchdog timeout 15 s (the shipped value): the same
  kill-early a QMP system_reset 0.5 s after the resize starts (the harness's `resize started` line), then a second boot
  kill-late  a QMP system_reset 6.0 s after the resize starts (about 70 % through the 2 TiB resize), then a second boot
             (--kill-at sets the delay for the kill modes)
  refuse     a 2 TiB disk, wd15: the grow is refused by name (grow-too-large) and the table stays as it was

The disk is a sparse raw file (2 TiB unless --size) with the MBR or GPT layout and the builder's data filesystem
(mke2fs -t ext4 -b 4096 -I 256 -m 0 -i 16384 -O meta_bg -J size=4 -L lemnos-data). The guest's init (the crate testing/crates/grow-live-init, --init) mounts /data, runs the harness,
unmounts and powers off. After the run the data partition's extents are copied sparsely out of the image and
e2fsck -fn (as the disk is) and e2fsck -fy (after the journal replay, which must change nothing) check the copy.
Nothing is run as root, and no loop device is used.
"""
import argparse, json, os, shutil, socket, subprocess, sys, threading, time, uuid

START_SECTOR = 169984          # partition 2 (the data partition) in the MBR layout the grow tests use
P1_START, P1_SECTORS = 8192, 161792
TIB = 1 << 40


def run(cmd, **kw):
    return subprocess.run(cmd, check=False, capture_output=True, text=True, **kw)


def make_disk(path, size_bytes, tree, table_kind="mbr"):
    if os.path.exists(path):
        os.remove(path)
    with open(path, "wb") as f:
        f.truncate(size_bytes)
    if table_kind == "gpt":
        table = (f"label: gpt\nstart={P1_START}, size={P1_SECTORS}, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=boot\n"
                 f"start={START_SECTOR}, size=77824, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=lemnos-data\n")
    else:
        table = (f"label: dos\nstart={P1_START}, size={P1_SECTORS}, type=c\n"
                 f"start={START_SECTOR}, size=77824, type=83\n")
    r = subprocess.run(["sfdisk", "--no-reread", "-q", path], input=table, text=True,
                       capture_output=True)
    if r.returncode:
        raise SystemExit(f"sfdisk: {r.stderr}")
    u = str(uuid.uuid4())
    # The image builder's contract (docs/slots.md): 4 KiB blocks, one inode per 16 KiB, meta_bg.
    r = run(["mke2fs", "-q", "-F", "-t", "ext4", "-b", "4096", "-I", "256", "-m", "0", "-i", "16384", "-O", "meta_bg,^resize_inode",
             "-U", u, "-E", f"hash_seed={u},root_owner=0:0,offset={START_SECTOR * 512}",
             "-J", "size=4", "-L", "lemnos-data", "-d", tree, path, "9728"])
    if r.returncode:
        raise SystemExit(f"mke2fs: {r.stderr}")


def make_tree(root):
    if os.path.isdir(root):
        shutil.rmtree(root)
    os.makedirs(os.path.join(root, "etc"))
    os.makedirs(os.path.join(root, "data"))
    for i in range(6):
        with open(os.path.join(root, "data", f"f{i}"), "w") as f:
            f.write(f"slot {i}\n")
    with open(os.path.join(root, "etc", "hosts"), "w") as f:
        f.write("localhost\n")


def make_initramfs(work, harness, init):
    root = os.path.join(work, "root")
    if os.path.isdir(root):
        shutil.rmtree(root)
    for d in ("dev", "proc", "sys", "data"):
        os.makedirs(os.path.join(root, d))
    shutil.copy(init, os.path.join(root, "init"))
    shutil.copy(harness, os.path.join(root, "lemnos-grow-host"))
    out = os.path.join(work, "initramfs")
    r = subprocess.run(f"cd '{root}' && find . -print0 | cpio --quiet --null -o -H newc | gzip -1 > '{out}'",
                       shell=True, capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"cpio: {r.stderr}")
    return out


class Qmp:
    """A QMP client on a unix socket: capabilities first, then commands."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        for _ in range(200):
            try:
                self.sock.connect(path)
                break
            except OSError:
                time.sleep(0.05)
        self.file = self.sock.makefile("rw")
        self.file.readline()  # greeting
        self.execute("qmp_capabilities")

    def execute(self, command):
        self.file.write(json.dumps({"execute": command}) + "\n")
        self.file.flush()
        while True:
            reply = json.loads(self.file.readline())
            if "return" in reply or "error" in reply:
                return reply


def gpt_target_bytes(size_bytes):
    """The grown GPT partition 2's length: the last whole MiB before the usable end (disk end - 34 LBAs)."""
    last_usable = size_bytes // 512 - 34
    target_end = ((last_usable + 1) // 2048) * 2048 - 1
    return (target_end + 1 - START_SECTOR) * 512


def copy_partition(image, out, length):
    """Copies the data extents of the image's partition 2 to `out` (sparse), from the partition's start.

    Positional reads and writes only (pread/pwrite at absolute offsets). An earlier version mixed
    os.lseek on the descriptor with a buffered file object's seek and read: a buffered seek inside its
    own buffer does not move the descriptor, so reads after a SEEK_HOLE came from the wrong place and
    the copy lost data (GROW2 finding: the check then reported a damaged directory block).
    """
    start = START_SECTOR * 512
    src = os.open(image, os.O_RDONLY)
    dst = os.open(out, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.ftruncate(dst, length)
        end = start + length
        pos = start
        while pos < end:
            try:
                data = os.lseek(src, pos, os.SEEK_DATA)
            except OSError:
                break  # no more data
            if data >= end:
                break
            hole = min(os.lseek(src, data, os.SEEK_HOLE), end)
            at = data
            while at < hole:
                chunk = os.pread(src, min(hole - at, 1 << 24), at)
                if not chunk:
                    raise OSError(f"short read of the image at {at}")
                off = 0
                while off < len(chunk):
                    off += os.pwrite(dst, chunk[off:], at - start + off)
                at += len(chunk)
            pos = hole
    finally:
        os.close(src)
        os.close(dst)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["wd3", "wd15", "kill-early", "kill-late", "refuse"])
    ap.add_argument("--kernel", required=True)
    ap.add_argument("--harness", required=True)
    ap.add_argument("--init", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--size", default="2T", help="the disk size: 2T or a number of bytes")
    ap.add_argument("--table", choices=["mbr", "gpt"], default="mbr", help="the partition table")
    ap.add_argument("--max-run", type=int, default=240, help="the harness's run limit, s")
    ap.add_argument("--kill-at", type=float, default=None,
                    help="kill modes: the reset's delay after the resize starts, s (default 0.5 early, 6.0 late)")
    args = ap.parse_args()

    size = 2 * TIB if args.size == "2T" else int(args.size)
    os.makedirs(args.work, exist_ok=True)
    image = os.path.join(args.work, "disk.img")
    tree = os.path.join(args.work, "tree")
    make_tree(tree)
    make_disk(image, size, tree, args.table)
    initramfs = make_initramfs(args.work, args.harness, args.init)
    wd = 3 if args.mode == "wd3" else 15
    # The partition the disk is expected to end with: the whole disk's end, or (refuse) the table's own
    # 38 MiB partition, which a refused grow leaves as it is.
    expect_bytes = None
    sock = os.path.join(args.work, "qmp.sock")
    if os.path.exists(sock):
        os.remove(sock)
    log_path = os.path.join(args.work, "console.log")
    cmd = ["qemu-system-x86_64", "-accel", "kvm", "-cpu", "max", "-smp", "2", "-m", "2048", "-nographic",
           "-kernel", args.kernel, "-initrd", initramfs,
           "-append", f"console=ttyS0 panic=-1 loglevel=4 printk.time=1 lemnos.wd={wd} lemnos.max={args.max_run}",
           "-drive", f"file={image},if=none,id=d,format=raw", "-device", "nvme,drive=d,serial=lemnosgrow2",
           "-device", "i6300esb", "-watchdog-action", "reset",
           "-qmp", f"unix:{sock},server,nowait"]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    qmp = Qmp(sock)
    lines, resets = [], []
    kill_at = {"kill-early": 0.5, "kill-late": 6.0}.get(args.mode)
    if kill_at is not None and args.kill_at is not None:
        kill_at = args.kill_at
    armed = {"resize_at": None, "done": False}
    log = open(log_path, "w")

    def reader():
        for line in proc.stdout:
            now = time.monotonic()
            lines.append(line.rstrip("\n"))
            log.write(f"{now:.3f} {line}")
            log.flush()
            if kill_at is not None and not armed["done"] and "grow-host: resize started" in line:
                armed["resize_at"] = now
    t = threading.Thread(target=reader, daemon=True)
    t.start()
    if kill_at is not None:
        while armed["resize_at"] is None and proc.poll() is None:
            time.sleep(0.02)
        if armed["resize_at"] is not None:
            time.sleep(max(0.0, armed["resize_at"] + kill_at - time.monotonic()))
            # The guest may have finished before the reset (a late kill point on a short resize): then
            # there is no reset, and the summary says so instead of the run failing.
            try:
                qmp.execute("system_reset")
                resets.append(time.monotonic() - armed["resize_at"])
            except (OSError, ValueError):
                pass
            armed["done"] = True
    try:
        proc.wait(timeout=1500)
    finally:
        t.join(timeout=10)
        log.close()
    starts = sum(1 for l in lines if "grow-init: start" in l)
    watchdog = any("Watchdog timer expired" in l or "watchdog" in l.lower() and "reset" in l.lower() for l in lines)
    probes = [l for l in lines if "latency probe" in l]
    finals = [l for l in lines if "finished=" in l]
    summary = {
        "mode": args.mode, "watchdog_s": wd, "guest_boots": starts, "exit": proc.returncode,
        "reset_after_resize_start_s": resets[0] if resets else None,
        "watchdog_reset_seen": watchdog, "probes": probes, "finals": finals,
        "resize_started": [l for l in lines if "resize started" in l],
        "grow_states": [l for l in lines if "grow_state=" in l],
    }
    # The disk, after the guest powered off: the data partition's copy, checked on the host.
    part_len = size - START_SECTOR * 512
    if args.mode == "refuse":
        part_len = 77824 * 512  # the table's partition 2 (38 MiB), unchanged by a refused grow
        expect_bytes = part_len
    elif args.table == "gpt":
        expect_bytes = gpt_target_bytes(size)
        part_len = size - START_SECTOR * 512
    else:
        expect_bytes = part_len - (part_len % 4096)
    copy = os.path.join(args.work, "part2.img")
    copy_partition(image, copy, part_len)
    # Two checks on the copy: -fn reads the filesystem as it is on the disk (the journal is NOT
    # replayed); -fy replays the journal and then checks, and must change nothing. Both outputs are
    # kept in full. The copy is clean only when the replayed check exits 0.
    fsck = run(["e2fsck", "-fn", copy])
    replay = run(["e2fsck", "-fy", copy])
    with open(os.path.join(args.work, "e2fsck-fn.txt"), "w") as f:
        f.write(fsck.stdout + fsck.stderr)
    with open(os.path.join(args.work, "e2fsck-fy.txt"), "w") as f:
        f.write(replay.stdout + replay.stderr)
    dump = run(["dumpe2fs", "-h", copy])
    blocks = next((l.split(":", 1)[1].strip() for l in dump.stdout.splitlines() if l.startswith("Block count")), None)
    inodes = next((l.split(":", 1)[1].strip() for l in dump.stdout.splitlines() if l.startswith("Inode count")), None)
    baseline = run(["debugfs", "-R", "ls /board/slots", copy]).stdout
    baseline_present = "grow" in baseline.split()
    summary.update({
        "e2fsck_fn_exit": fsck.returncode, "e2fsck_fy_exit": replay.returncode,
        "e2fsck_fy_tail": replay.stdout.strip().splitlines()[-3:],
        "block_count": blocks, "inode_count": inodes,
        "partition_bytes": part_len, "fs_bytes": int(blocks) * 4096 if blocks else None,
        "baseline_file_present": baseline_present,
    })
    with open(os.path.join(args.work, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary, indent=2))
    os.remove(copy)
    ok = replay.returncode == 0 and int(blocks or 0) * 4096 == expect_bytes and not baseline_present
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
