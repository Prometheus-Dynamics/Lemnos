# BB2: the U-Boot restart count and episodes (untested on board)

Not run on a board. Host side only: the script's count, rung and rescue lines run over the Rust evaluator
(`drivers/system-slots/src/uboot/tests.rs`), the environment writer's clear is cut at every byte
(`drivers/slot-store/src/uboot_env_tests.rs`), and the lemnosd arrival and good-boot paths run over the
writer's environment image.

## Checks that need a board (none run)

1. A warm restart raises `lemnos_restarts` (`setexpr.l` of the reset word, then one `saveenv`), and a
   power-on boot sets it to 0. This needs a board whose SoC has a cited reset word. None does today
   (`docs/boards.md`, `uboot_reset_word`), so this check is blocked until a citation exists.
2. Rescue at 8: download mode with no slot started, and the host flasher's default switch clearing the count.
3. Wear: the writes per healthy boot (zero for power-on, two for warm) on the board's eMMC.

## Stop and return path

Every check above changes only the environment's count and the episodes. A board left at a count of 8 waits
in download mode for the host (`fastboot continue` returns to the loop, which stays at rescue until the count
is cleared by a default switch or a good boot). The return path is the host's default switch, tested on the
host side first. Nothing here changes the slots' files.
