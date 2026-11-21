# PWM-SYSFS-UNIT: the root-phase PWM grant under the packaged unit

Status: the grant mechanism is proven in a QEMU guest under a real systemd 259.10 unit, with a stand-in attribute.
The PWM chip itself is **untested on board**: the guest has no PWM chip, and no board has run this.

## Design (as built)

- The unit is unchanged. The daemon stays `lemnos`, with the base unit's `ExecStart`, bounding set, capabilities and
  `ProtectKernelTunables=yes`. There is no drop-in.
- Before the drop, the root phase's `pwm channels` step (`service/lemnosd/src/privilege.rs`, `pwm::grant` in
  `backends/linux/src/pwm.rs`) exports each named channel the kernel has not exported, waits a bounded time for `pwmM`,
  then chowns `period`, `duty_cycle`, `polarity` and `enable` to root:`lemnos`, mode 0660. `export` and `unexport`
  stay root's, so the daemon never exports or unexports; a channel stays exported for the boot.
- A channel or chip that cannot be granted refuses its device by name; the daemon keeps running.
- The write the daemon makes is limited by the kernel's permission check on those four attributes.

## Why there is no ReadWritePaths (the first design was wrong)

The first design added `ReadWritePaths=-/sys/devices/platform` on the reading that `ProtectKernelTunables=yes` makes
`/sys` read-only. The guest shows it does not, under the unmodified 259.10 unit (below). The cause is in the source
(systemd v259.10, the tag the guest runs, `src/core/namespace.c`; the enum at lines 62–85; sha256 of `systemd-v259.10.tar.gz` from the
`systemd/systemd` tag `v259.10` archive: `f9a3f1e21e4dd78717bad0888638954467a55d73cab44a2c7a75c5bed47ef814`):

- line 169, `protect_kernel_tunables_sys_table`: `{ "/sys", MOUNT_READ_ONLY, false }` (the entry that would make `/sys` ro);
- line 146, `apivfs_table`: `{ "/sys", MOUNT_BIND_SYSFS, false }`, added because ProtectKernelTunables implies MountAPIVFS (line 2248);
- line 1047, `drop_duplicates`: when two entries share a path, the first in `mount_path_compare` order wins; modes sort by
  enum (`MOUNT_BIND_SYSFS` at line 72 is listed before `MOUNT_READ_ONLY` at line 75, so it sorts first);
- line 1433, `mount_bind_sysfs`: "make this a NOP if /sys is already a mount point"; the namespace's /sys is one, so the
  entry is not applied;
- line 2403, `m->state = r == 0 ? MOUNT_SKIPPED : MOUNT_APPLIED`, so the entry is MOUNT_SKIPPED;
- line 2147, `make_read_only`: `if (m->state != MOUNT_APPLIED) return 0;`, so the read-only flag is never set.

The same code is in 258 (`MOUNT_BIND_SYSFS` NOP at line 1433, `MOUNT_SKIPPED` at line 2403, the early return at 2142).
`/sys/kernel/debug` and `/sys/kernel/tracing` are separate table entries and are read-only (the probe does not test them).

## Guest proof: real systemd unit in QEMU, stand-in attribute

Label: **real systemd unit in QEMU, stand-in attribute.** The guest runs the host's systemd 259.10 as PID 1, the host
kernel, and the packaged `lemnosd.service` (unchanged) with ExecStart replaced by a probe. The probe runs as root,
chowns `/sys/devices/platform/LNXPWRBN:00/power/control` to root:lemnos 0660 (the stand-in for the root phase's
grant; the first `power/control` the guest's platform devices list), leaves
`/sys/devices/platform/PNP0103:00/power/control` root's, then writes both as uid 990 (lemnos) inside the unit.

Run 5 (`sysguest/work/out/console.log`, the scratch copy of the stanza; the stanza is not committed):

- Namespace: the unit's mount namespace is its own (`/proc/self/ns/mnt` = `mnt:[4026532119]`, a fresh id for each unit);
  `/proc/sys` is `ro` in its mountinfo, so ProtectKernelTunables was applied. PID 1's inode could not be read from the unit
  (`ProtectProc=invisible` hides it), so this is not a direct comparison with PID 1.
- `/sys` mountinfo: `… /sys rw,nosuid,nodev,noexec,relatime shared:14 master:3 - sysfs sysfs rw` (field 6 is `rw`).
- `systemctl show lemnosd` from the guest: `ProtectKernelTunables=yes ProtectSystem=strict NoNewPrivileges=yes
  PrivateDevices=no ReadWritePaths= MountFlags=` (empty).
- The granted attribute: `root:lemnos 660`; the ungranted one: `root:root 644`.
- Writes as uid 990 (`uid=990`): the granted write succeeds; the ungranted write is refused with EACCES
  (`Permission denied`).
- Result: **PASS** for both checks, with the unmodified unit. The fail-before this item planned (the EROFS case) does not
  exist on 259.10: `/sys` is writable under the base unit.
- Power-off works (`sysrq: Power Off`; the guest exits without the bound).

Paths (scratch, not committed): `/run/user/1000/tp/PWM-SYSFS-UNIT/sysguest/` (runner `run-pwm-unit.sh`, probe
`guest-new/pwm-probe.sh`, logs `work/out/console.log`, `guest5-run.log`). The runner's verdict line matched the probe lines
with `^`, which console prefixes defeat; read the PASS lines from the console directly.

## Board check (not yet run)

On a board with a PWM channel:
1. `systemctl show lemnosd.service -p ExecStart -p ReadWritePaths -p ProtectKernelTunables -p ProtectSystem -p User` —
   ExecStart is lemnosd with `--user lemnos`; no ReadWritePaths is needed.
2. Mount table: field 6 of `/proc/<MainPID>/mountinfo` for `/sys` (expect `rw` on 259.10; the host's mixed results
   were also `rw` for several services, and `ro` for two; field 6 decides).
3. The root phase's log line: `lemnosd: root phase: <device>: <need>: <real dir>: granted to 0:<gid>`.
4. `stat -c '%U:%G %a'` on `period`, `duty_cycle`, `polarity`, `enable`: `root:lemnos 660`; `export`, `unexport`: root's.
5. A write by the `lemnos` user to a granted attribute succeeds (`setpriv --reuid=<lemnos> --regid=<lemnos> --clear-groups`);
   an `export` write by the same user is refused (EACCES).
6. The fan's duty changes on the channel's `duty_cycle`, read back as root.

Record the systemd version (`systemctl --version`), the `systemctl show` lines, the mountinfo lines and the outcomes.

## Host evidence

On this host (systemd 259.10), `accounts-daemon` and `polkit` (`ProtectKernelTunables=yes`, no `ReadWritePaths=` on `/sys`,
`PrivateUsers=no`) show `/sys ro`. Other services with the same setting show `/sys rw`. The source explains the rw case
(above); the host's ro cases are not explained, and field 6 on the board is the check.

## Not run

- systemd 258: the source is the same for these paths; no 258 guest run.
- A real `lemnosd` binary in the guest (the probe stands in for it).
- A PWM chip, in the guest or on a board.
