# WAY-BACK-TOOL board run

Status: untested on board. Nothing here has touched a board. The tool is proven in QEMU only, on the
firmware mailbox stand-in (`testing/qemu/test_block/lemnos_test_vcio.rs`): the `way-back` stanza of
`testing/qemu/run.sh` (`--only way-back`, kvm, on next 7b4fdb25 plus this change) passed every check:

- no reboot order before; `lemnos-way-back rpiboot` exits 0 and the stand-in took one order, RPIBOOT (3);
- with the stand-in refusing (`refuse=1`, answer word `0x80000001`), exit 1, and the kernel log has
  `lemnos-way-back: the way back is not armed: RPIBOOT through /dev/vcio: the firmware did not take the
  reboot order: answer word 0x80000001`;
- `--device /dev/lemnos-no-mailbox`: exit 1, `No such file or directory (os error 2)` in the kernel log;
- the order count is still 1 after both failures.

The stand-in is not the firmware: there is no next boot for an order to apply to.

## Checks to run on the board (a BCM2712 board, RAM-booted image or the board's own)

1. In the initramfs (before any device manager), as root: `ls -l /dev/vcio` exists (devtmpfs).
2. `lemnos-way-back rpiboot` prints `the firmware took RPIBOOT for the next boot` and exits 0; `dmesg`
   has the same line.
3. Restart (`reboot -f`, or let the watchdog fire): the board comes up in RPIBOOT and the PC's `rpiboot`
   sees it. Power-cycle: the stored `BOOT_ORDER` runs again (the order was for one boot).
4. Run it as a non-root user: exit 1, `Permission denied` named, and a normal restart boots as before.
