# REBOOT-TO-RPIBOOT: the restart into the board's way back, on request

Status: **untested on board.** The guest proof below ran on the PC against a stand-in mailbox. No board has
run `reboot_to`.

## What is built

- `contracts/system-reboot`: method 2 `reboot_to(target)`, `#[added]` (the base hash `0xface08cdca5b68c0` does not move).
- `engine/host`: the board file's `[boot] loop_return` is given as the `loop_return` setting to every device that serves
  `system.reboot/`, and to no other; a restart device that names it in its own config is refused at load.
- `drivers/system-slots` (`SystemReboot::reboot_to`): offered only where the board declares the way back. Refusals, each
  asking and restarting nothing: `NOT_SUPPORTED` (no way back, or no firmware for the record), `INVALID_ARGUMENT` (another
  target), `NOT_PERMITTED` (security switch on), `IO` (the restart helper's check refuses, or the mailbox refuses). Then the
  one-shot (`TAG_SET_REBOOT_ORDER`, 3) through the mailbox, the request into the boot history, and the restart through the
  restart helper.
- `drivers/system-slots` `reboot_root_phase`, wired in `service/lemnosd/src/privilege.rs` `ROOT_PHASES`: the restart device
  holds `vcio` across the drop (root:root 0600 on a board), as the slots device does for the boot count.

## Proof on the PC

- Unit tests, `drivers/system-slots/src/reboot_to_tests.rs` (8), `engine/host` (`the_way_back_goes_to_the_restart_devices_only_and_one_of_their_own_is_refused`),
  `contracts/system-reboot/tests/contract.rs` (the method is added, the base hash is `reboot` alone).
- QEMU, `testing/qemu/run.sh --only route-a`: the stand-in mailbox `lemnos_test_vcio.ko` makes `/dev/vcio` root:root 0600.
  The real lemnosd under the packaged unit, dropped to its own user, with a `system-reboot` device and
  `[boot] loop_return = "rpiboot"`: the root phase holds the node; the description lists `reboot_to` (2); `reboot_to("uboot")`
  is `INVALID_ARGUMENT` with nothing asked; `reboot_to("rpiboot")` gives the stand-in one order of 3 and the helper is asked
  check, check, start; with the stand-in refusing, the answer is `IO`, no order is taken and the helper is asked its check only.

## The board step (for the board session's sheet)

Needs: a CM5 board on an image built from this rev, whose board file has `[boot] loop_return = "rpiboot"` and a
`system-reboot` device, the security switch off, and a host-side RPIBOOT listener (the flasher's USB host) on the
board's USB port. This restarts the board into RPIBOOT: it leaves its default slot for the flashing mode, so it is a
board step with a return path, not a read-only one.

1. Take the board's locks. Read and record: `lemnosctl list` (the `reboot` device ok), the description of `reboot`
   (method 2 `reboot_to` listed), and the daemon's log line `root phase: reboot: /dev/vcio: held for the restart`.
2. `reboot_to("uboot")`: expect `INVALID_ARGUMENT`; the board stays up.
3. With the security switch on (if the board's switch can be turned on and back off in this run), `reboot_to("rpiboot")`:
   expect `NOT_PERMITTED`; the board stays up. Turn the switch back off. Skip and say so if not.
4. `reboot_to("rpiboot")`: expect the call to be taken, the board to restart, and the host to see the RPIBOOT device
   (the Broadcom boot ROM's USB id) within the restart's bound. The boot history after the return names the caller
   (`requested = by:<caller>`).
5. Return: the host's flasher boots the board back to its default slot (no write), or a power cycle does (the one-shot
   is for one boot only). Record the default slot after: unchanged.

Pass: 2 and 4 as expected (3 when run); the default slot and config.txt unchanged after the return.
