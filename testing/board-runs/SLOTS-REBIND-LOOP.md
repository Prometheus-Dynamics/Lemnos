# SLOTS-REBIND-LOOP: boot good off the tick, once a boot

Untested on board. The check below needs a slot board (the CM5 on the firmware form) with
lemnosd from this change installed.

## What it fixes

On slow storage the system-slots driver's boot good (device baseline, boot-file verify, count
clear), run on the tick 30 s after each bind, took longer than the 2 s call deadline. The tick was
abandoned, the device rebound, and the boot good started again 30 s later: a loop of about 36 s.
It also ran again 30 s after every daemon restart.

## The check

Under the board lock, Raze #2, read-only apart from what lemnosd itself writes:

1. After a boot, wait 40 s. `lemnos-ctl call slots info` shows `"boot_good":"ran this boot for
   <running slot> at <secs> s, by this daemon"`. `cat /run/lemnos/boot-good` shows `boot` equal to
   `/proc/sys/kernel/random/boot_id`, the running slot, and the same seconds.
2. `journalctl -u lemnosd -b` over 5 minutes from boot: no `slots` rebind and no "abandoned" line
   for the slots device; one boot good only.
3. `systemctl restart lemnosd` (the operator's restart is allowed on the bench board; the watchdog
   stays armed). After 40 s, `info` shows `by an earlier daemon of this boot` with the first time,
   and no new verify ran: no burst of boot-partition reads (`/sys/block/mmcblk0/stat` read
   sectors unchanged across the 30 s mark, within the daemon's own small reads) and no mailbox
   `TAG_SET_BOOT_COUNT` (the driver's log has no "boot count is cleared" line after the restart).
4. Report the CM5 timings: the time from bind to the boot good's mark, and the longest
   slots tick in that window if the host logs one.

## Results

Not run yet.
