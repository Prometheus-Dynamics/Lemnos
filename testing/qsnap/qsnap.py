#!/usr/bin/env python3
"""qsnap: boot a group of QEMU guests once, keep their state, and restore it in a second or two.

    qsnap.py save SPEC [--cache DIR]                      boot the group to its ready point and keep it
    qsnap.py run  SPEC [--cache DIR] [--send LINE] [--payload FILE] [--after CMD] [--logs DIR]

SPEC is a JSON file:

    {"guests": [{"name": "board", "argv": ["qemu-system-x86_64", "-accel", "kvm", ...],
                 "inputs": ["path/to/bzImage", "path/to/initramfs"]}],
     "ready": "guest" | {"cmd": ["host", "command"]},
     "payload_mb": 1024, "timeout_s": 600,
     "disks": {"board": {"disk0": "path/to/base.img"}}}

- The state is kept under DIR/<key>/, where the key is the hash of this file, the QEMU version and the
  spec, with the content of every file named in "inputs". A key that is there is used as it is: an
  unchanged group is never booted twice. Writers build in DIR/<key>.<pid> and move it into place.
- qsnap adds to each guest a QMP socket, a control port (virtconsole, the guest's /dev/hvc0) and, with
  "payload_mb", a read-only virtio-blk payload disk (serial qsnap-payload) of that fixed size.
- Ready: "guest" waits for each guest to write `QSNAP READY` on its control port; {"cmd": [...]} runs
  the host command until it exits 0 (for guests that are reached over the network).
- Disks a guest writes: "disks" names, per guest, a base image (raw) for each disk id. `{disk:ID}` in
  that guest's argv is a qcow2 overlay over the base, made for each run; the base is never written
  (list it in "inputs" too: the kept state holds the overlay, which only matches that base). A save
  keeps each overlay beside the memory state, and a restore starts from a copy of it (a reflink where
  the filesystem has them), so no run's writes reach another's.
- run: restores every guest (-incoming), runs --after (a host command, for links a restore drops:
  vsock connections are reset by design), then writes LINE to each control port and passes the
  consoles through. The payload disk is FILE, padded to the fixed size: its content is not in the
  state, so it may change from run to run as long as the guest reads it only after the restore.
- A guest that writes `QSNAP SAVE <name>` on its control port during a run is stopped, its group's
  state kept under DIR/<key>-<name>-<hash of payload and LINE>/, and continued; it reads
  `QSNAP SAVED <dir name>` back, and qsnap prints the same line. A later run restores that state with
  --from <dir name>; the guest, still waiting for its answer, reads that run's LINE instead: a guest
  tells the two apart by the line (the run that saved it gets `QSNAP SAVED ...`).

Exit status: the highest of the guests' exit statuses; 124 for a guest past timeout_s.
Generic: nothing here knows what the guests run.
"""
import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time

VERSION = "1"


def die(msg, code=2):
    print(f"qsnap: {msg}", file=sys.stderr)
    sys.exit(code)


class Qmp:
    """A QMP connection: one command at a time, events kept as they come."""

    def __init__(self, path, within=30.0):
        end = time.monotonic() + within
        while True:
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(path)
                break
            except OSError:
                self.s.close()
                if time.monotonic() > end:
                    die(f"no QMP at {path}")
                time.sleep(0.02)
        self.f = self.s.makefile("rwb", buffering=0)
        self.events = []
        self._read()  # greeting
        self.cmd("qmp_capabilities")

    def _read(self):
        line = self.f.readline()
        if not line:
            raise EOFError("QMP closed")
        return json.loads(line)

    def cmd(self, name, **args):
        msg = {"execute": name}
        if args:
            msg["arguments"] = args
        self.f.write(json.dumps(msg).encode() + b"\n")
        while True:
            r = self._read()
            if "event" in r:
                self.events.append(r)
                continue
            if "error" in r:
                raise RuntimeError(f"{name}: {r['error'].get('desc')}")
            return r.get("return")

    def wait_migration(self, within=300.0):
        end = time.monotonic() + within
        while time.monotonic() < end:
            st = self.cmd("query-migrate").get("status")
            if st == "completed":
                return
            if st in ("failed", "cancelled"):
                raise RuntimeError(f"migration {st}")
            time.sleep(0.02)
        raise RuntimeError("migration did not finish")

    def wait_incoming(self, within=300.0):
        end = time.monotonic() + within
        while time.monotonic() < end:
            st = self.cmd("query-status").get("status")
            if st in ("paused", "postmigrate") and self.cmd("query-migrate").get("status") == "completed":
                return
            if self.cmd("query-migrate").get("status") == "failed":
                raise RuntimeError("incoming migration failed")
            time.sleep(0.02)
        raise RuntimeError("incoming migration did not finish")


def file_hash(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def spec_key(spec, spec_text):
    h = hashlib.sha256()
    h.update(open(__file__, "rb").read())
    h.update(spec_text.encode())
    for g in spec["guests"]:
        version = subprocess.run([g["argv"][0], "--version"], capture_output=True, text=True).stdout
        h.update(version.encode())
        for p in g.get("inputs", []):
            h.update(file_hash(p).encode())
    return h.hexdigest()[:32]


class Guest:
    def __init__(self, g, work, payload_mb, payload=None, incoming=False, disks=None, kept=None):
        self.name = g["name"]
        self.work = work
        self.qmp_path = os.path.join(work, f"{self.name}.qmp")
        self.ctl_path = os.path.join(work, f"{self.name}.ctl")
        # Each disk the guest writes: an overlay for this run, made over the base or copied from
        # the kept state's overlay (`kept`, a directory).
        self.overlays = {}
        for disk, base in (disks or {}).items():
            overlay = os.path.join(work, f"{self.name}-{disk}.qcow2")
            if kept:
                subprocess.run(["cp", "--reflink=auto", os.path.join(kept, f"{self.name}-{disk}.qcow2"), overlay], check=True)
            else:
                subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b", os.path.abspath(base), overlay], check=True)
            self.overlays[disk] = overlay
        argv = [self.with_disks(a) for a in g["argv"]] + [
            "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
            "-chardev", f"socket,id=qsnapctl,path={self.ctl_path},server=on,wait=off",
            "-device", "virtio-serial-pci,id=qsnapser",
            "-device", "virtconsole,chardev=qsnapctl,id=qsnapcon",
        ]
        if payload_mb:
            disk = os.path.join(work, f"{self.name}.payload")
            with open(disk, "wb") as out:
                if payload:
                    with open(payload, "rb") as src:
                        shutil.copyfileobj(src, out)
                if out.tell() > payload_mb << 20:
                    die(f"payload {payload} is larger than payload_mb ({payload_mb} MiB)")
                out.truncate(payload_mb << 20)
            argv += ["-drive", f"file={disk},if=none,id=qsnappay,format=raw,readonly=on",
                     "-device", "virtio-blk-pci,drive=qsnappay,serial=qsnap-payload"]
        if incoming:
            argv += ["-incoming", "defer"]
        self.log = open(os.path.join(work, f"{self.name}.console"), "ab")
        self.proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=self.log, stderr=subprocess.STDOUT)
        self.qmp = Qmp(self.qmp_path)
        self.ctl = None

    def with_disks(self, arg):
        for disk, overlay in self.overlays.items():
            arg = arg.replace(f"{{disk:{disk}}}", overlay)
        return arg

    def control(self, within=30.0):
        if self.ctl is None:
            end = time.monotonic() + within
            while True:
                try:
                    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    s.connect(self.ctl_path)
                    self.ctl = s.makefile("rwb", buffering=0)
                    break
                except OSError:
                    s.close()
                    if time.monotonic() > end:
                        die(f"{self.name}: no control port")
                    time.sleep(0.02)
        return self.ctl

    def read_line(self):
        """The next line from the guest, from its first `QSNAP ` on. The port is a terminal that
        echoes what qsnap writes, so a line may start with echoed characters, or be only an echo."""
        line = self.control().readline()
        if not line:
            return None
        line = line.decode(errors="replace").strip()
        at = line.find("QSNAP ")
        return line[at:] if at >= 0 else line

    def say(self, line):
        self.control().write(line.encode() + b"\n")


def keep_state(guests, dst):
    """Stops every guest, migrates each to dst/<name>.state, and moves dst into place."""
    part = f"{dst}.{os.getpid()}"
    shutil.rmtree(part, ignore_errors=True)
    os.makedirs(part)
    for g in guests:
        g.qmp.cmd("stop")
    for g in guests:
        g.qmp.cmd("migrate", uri=f"file:{part}/{g.name}.state")
    for g in guests:
        g.qmp.wait_migration()
    # The guests are stopped, their disks flushed by the migration: each overlay as it stands now.
    for g in guests:
        for disk, overlay in g.overlays.items():
            subprocess.run(["cp", "--reflink=auto", overlay, os.path.join(part, f"{g.name}-{disk}.qcow2")], check=True)
    try:
        os.rename(part, dst)
    except OSError:  # another writer kept the same state first
        shutil.rmtree(part, ignore_errors=True)


def wait_ready(spec, guests, within):
    ready = spec.get("ready", "guest")
    end = time.monotonic() + within
    if ready == "guest":
        for g in guests:
            while True:
                line = g.read_line()
                if line is None:
                    die(f"{g.name}: control port closed before QSNAP READY")
                if line == "QSNAP READY":
                    break
                if time.monotonic() > end:
                    die(f"{g.name}: not ready within {within} s")
    else:
        while subprocess.run(ready["cmd"]).returncode != 0:
            if time.monotonic() > end:
                die(f"not ready within {within} s")
            time.sleep(0.2)


def load(path):
    text = open(path).read()
    return json.loads(text), text


def save(args):
    spec, text = load(args.spec)
    key = spec_key(spec, text)
    dst = os.path.join(args.cache, key)
    if os.path.isdir(dst):
        print(key)
        return 0
    os.makedirs(args.cache, exist_ok=True)
    work = os.path.join(args.cache, f".work-{os.getpid()}")
    os.makedirs(work, exist_ok=True)
    guests = []
    try:
        started = time.monotonic()
        guests = [Guest(g, work, spec.get("payload_mb"), disks=spec.get("disks", {}).get(g["name"])) for g in spec["guests"]]
        wait_ready(spec, guests, spec.get("timeout_s", 600))
        keep_state(guests, dst)
        print(f"qsnap: kept {key} after {time.monotonic() - started:.1f} s", file=sys.stderr)
    finally:
        for g in guests:
            try:
                g.qmp.cmd("quit")
            except Exception:
                pass
            g.proc.wait()
        shutil.rmtree(work, ignore_errors=True)
    print(key)
    return 0


def run(args):
    spec, text = load(args.spec)
    key = spec_key(spec, text)
    if not os.path.isdir(os.path.join(args.cache, key)):
        save(args)
    state = os.path.join(args.cache, key)
    if args.from_:
        if not args.from_.startswith(key + "-"):
            die(f"--from {args.from_}: not a state of this spec (its key is {key})")
        state = os.path.join(args.cache, args.from_)
        if not os.path.isdir(state):
            die(f"no state {state}: the run that saves it has to run first")
    work = os.path.join(args.logs or args.cache, f".run-{os.getpid()}")
    os.makedirs(work, exist_ok=True)
    timeout = spec.get("timeout_s", 600)
    started = time.monotonic()
    guests = [Guest(g, work, spec.get("payload_mb"), args.payload, incoming=True,
                    disks=spec.get("disks", {}).get(g["name"]), kept=state) for g in spec["guests"]]
    for g in guests:
        g.qmp.cmd("migrate-incoming", uri=f"file:{state}/{g.name}.state")
    for g in guests:
        g.qmp.wait_incoming()
    for g in guests:
        g.qmp.cmd("cont")
    print(f"qsnap: restored {os.path.basename(state)} in {time.monotonic() - started:.2f} s", file=sys.stderr)
    if args.after:
        r = subprocess.run(args.after, shell=True)
        if r.returncode != 0:
            die(f"--after exited {r.returncode}", 1)
    # Each control port is served on its own thread: the line to send, then saves the guest asks for.
    def serve(g):
        if args.send is not None:
            g.say(args.send)
        while True:
            line = g.read_line()
            if line is None:
                return
            if os.environ.get("QSNAP_DEBUG"):
                print(f"qsnap: {g.name} control: {line!r}", file=sys.stderr, flush=True)
            if line.startswith("QSNAP SAVE "):
                name = line.split(" ", 2)[2]
                dst = os.path.join(args.cache, f"{key}-{name}-{run_hash(args)}")
                if not os.path.isdir(dst):
                    keep_state(guests, dst)
                    for other in guests:
                        other.qmp.cmd("cont")
                g.say(f"QSNAP SAVED {os.path.basename(dst)}")
                print(f"QSNAP SAVED {os.path.basename(dst)}", file=sys.stderr, flush=True)
    threads = [threading.Thread(target=serve, args=(g,), daemon=True) for g in guests]
    for t in threads:
        t.start()
    status = 0
    for g in guests:
        try:
            status = max(status, g.proc.wait(timeout=max(1, timeout - (time.monotonic() - started))))
        except subprocess.TimeoutExpired:
            g.proc.kill()
            g.proc.wait()
            status = 124
    for g in guests:
        g.log.close()
        if args.logs:
            print(f"==> {g.name}: {os.path.join(work, g.name + '.console')}")
        else:
            sys.stdout.write(open(os.path.join(work, f"{g.name}.console"), errors="replace").read())
    if not args.logs:
        shutil.rmtree(work, ignore_errors=True)
    return status


def run_hash(args):
    h = hashlib.sha256()
    if args.payload:
        h.update(file_hash(args.payload).encode())
    h.update((args.send or "").encode())
    return h.hexdigest()[:16]


def main():
    p = argparse.ArgumentParser(prog="qsnap.py")
    sub = p.add_subparsers(dest="what", required=True)
    for name in ("save", "run"):
        s = sub.add_parser(name)
        s.add_argument("spec")
        s.add_argument("--cache", default=os.environ.get("QSNAP_CACHE", "cache/qsnap"))
        if name == "run":
            s.add_argument("--send")
            s.add_argument("--payload")
            s.add_argument("--after")
            s.add_argument("--logs")
            s.add_argument("--from", dest="from_")
    args = p.parse_args()
    if args.what == "save":
        args.payload = None
        args.send = None
        return save(args)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
