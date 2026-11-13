# Q2i board run: the daemon's device permissions from the generated udev fragment

Untested on board. Not run on any board: the checks below are written, not run. The privilege drop
was last measured on the reference board with the nodes changed by hand (F2e, `testing/board-runs/F2e.md`);
this run replaces that step with the fragment.

## What to run (one board, one daemon at a time)

Board state before: `/dev/lemnos*`, `/dev/dma_heap/linux,cma` root:root mode 600 (the stock state).

1. Write the fragment from the board's file: `lemnos-ctl tree /etc/lemnos/board.toml --udev /tmp/71-lemnos-board.rules`.
   Compare with the golden `service/devtree/tests/golden/reference-udev.rules` when the file is the reference board.
2. Install it as `/usr/lib/udev/rules.d/71-lemnos-board.rules`, `udevadm control --reload`, `udevadm trigger`.
   Expect `/dev/lemnos`, `/dev/lemnos-rp1-csi2-*` and `/dev/dma_heap/linux,cma` as `root:lemnos` mode 0660.
   Record `stat -c '%U:%G %a %n'` of each node before and after.
3. Start the daemon as the unit does (`lemnosd --user lemnos --group lemnos`) with the fragment installed.
   Expect it to start: the start check passes. Record the start log.
4. Stop the daemon by its own PID. Remove the fragment and reload udev. Start again.
   Expect a refusal naming the first node the group cannot read and write, with the words
   `not readable/writable by group lemnos: install the generated udev fragment`. Record the refusal.
5. Restore the fragment, reload, start, and run one stream as the `lemnos` user with
   `lemnos-camera-proof --sensor cam0-sensor --frames 300`: frames and their count, and that no
   manual `chgrp`/`chmod` was needed. Record the frame count.

Each check on the board is by PID for every process started (no `pkill`, no pattern stops).

## Not checked by this run

- The boot partition's block node: its rule is the I5i design's, not in this fragment.
- A board without `[host] dma_heaps`: the default `linux,cma` is what the fragment gives; a board that
  allocates from `system` names it in `dma_heaps` or in a stream.
