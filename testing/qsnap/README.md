# qsnap

Boot a group of QEMU guests once, keep their state, and restore it in under a second for every run.
`qsnap.py` is generic: it knows nothing about what the guests run, and needs only Python 3 and QEMU.

```text
qsnap.py save SPEC [--cache DIR]
qsnap.py run  SPEC [--cache DIR] [--send LINE] [--payload FILE] [--after CMD] [--from STATE] [--logs DIR]
```

The spec names each guest's QEMU command line and the files its state depends on:

```json
{"guests": [{"name": "board", "argv": ["qemu-system-x86_64", "-accel", "kvm", "-kernel", "bzImage", "..."],
             "inputs": ["bzImage", "initramfs"]}],
 "ready": "guest", "payload_mb": 1024, "timeout_s": 600}
```

- **Key.** A state is kept under `DIR/<key>/`, the key being the hash of `qsnap.py`, the spec, each
  guest's QEMU version and the content of every file in `inputs`. A state that is there is used: an
  unchanged group is never booted twice. `run` makes it first when it is missing.
- **Added to each guest:** a QMP socket, a control port (a virtconsole: the guest's `/dev/hvc0`) and,
  with `payload_mb`, a read-only virtio-blk disk of that fixed size with serial `qsnap-payload`.
- **Ready.** `"guest"`: the guest writes `QSNAP READY` on its control port and then reads one line, the
  run's. `{"cmd": [...]}`: a host command run until it exits 0, for guests reached over a network
  (for example a client asking the guest's service until it answers).
- **Payload.** `--payload FILE` is the payload disk for this run, padded to `payload_mb`. Its content
  is not in the kept state, so it changes from run to run: the code under test goes there, and a code
  change costs a restore, not a boot. The guest must not read the disk before the state is kept.
- **Disks a guest writes.** `"disks": {"<guest>": {"<id>": "<base image>"}}` and `{disk:<id>}` in that
  guest's argv (`-drive file={disk:d0},format=qcow2,...`): each run writes a qcow2 overlay of its own
  over the base, which is never written. A save keeps the overlays with the memory state; every
  restore starts from a copy (a reflink where the filesystem has them). List the base in `inputs`.
- **Mid-run states.** A guest that writes `QSNAP SAVE <name>` is stopped with its whole group, the
  group's state kept as `<key>-<name>-<hash of the payload and the line>`, and continued; it reads
  `QSNAP SAVED <that name>`, which qsnap also prints. `run --from <that name>` restores it: the guest,
  still waiting for its answer, reads that run's line instead. A test sets up once, saves, and every
  step after it restores from there, side by side.
- **Links a restore drops.** vhost-vsock resets every connection after a migration, by design.
  `--after CMD` runs on the host once every guest runs again, to set such links up again.
- **The control port echoes.** It is a terminal: what qsnap writes comes back. qsnap reads each line
  from its first `QSNAP ` on.

The guest's side, for guests whose first process is a Rust program: `testing/crates/qsnap-guest`
(`ready`, `save`, `unpack_payload`, the payload being a `newc` archive).

`check.sh` is qsnap's own check (`KERNEL=<x86_64 bzImage> testing/qsnap/check.sh`): lines, payloads,
mid-run states, key reuse and disk overlays. On this machine (KVM, load about 100): boot to ready and
keep 6.3 s; restore and run 1.2 s (the restore itself 0.44 s); restore of a mid-run state and run 1.5 s;
the whole check 46 s, a guest rebuild and two boots included.
