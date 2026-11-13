# U1b board run: the configuration buffer rules and the ISP layouts

Not run on the board yet. Untested on board.

What changed since D17: the `camera.isp/1` contract's configuration buffer rules (its module docs,
"Configuration buffers") and the two layout methods, `input_layout` (13) and `output_layout` (14).
The back end's job check now refuses a configuration whose input size or stride differs from the
frame `set_input` described, and a job naming a configuration buffer that an accepted job still
holds is refused. Both are host-tested on the mock; nothing here has run on the block.

## Setup

As in D17: the board lock, the CM5, builds on NVMe or SSD. Use `lemnos-isp-proof` with the colour
bars frame.

## Checks

1. Layouts. After `set_input` and `set_output(0, NV12)`, `input_layout` and `output_layout(0)` equal
   the stride and size the proof computes for the frame (2560 bytes a line and 2048000 bytes for
   1280x800 RAW16; the NV12 output's stride and size from `set_output`). Expect equal, and
   `output_layout(0)` `None` after `clear_output`.
2. Input geometry. A configuration built for a different input size, posted after `set_input`, is
   dropped and no frame is published. The next job with the right configuration runs.
3. Configuration buffers. Two jobs naming the same configuration buffer back to back: the second is
   refused (counter `dropped` +1) while the first is in flight; after the first completes, a job
   naming that buffer runs. Expect the frame of each job that runs to be the one its configuration
   describes.
4. Rewrite after completion. Write a different configuration into a buffer after its job's frame has
   been released, post a job naming it. Expect the new configuration's frame.

`lemnos-isp-proof --checks 1` now rewrites a configuration only after its job has completed, and
checks that the next job runs the rewritten one (contract rule 3). The earlier check wrote while the
job was in flight, which rule 1 forbids; that check is removed. Record here that the new check passes.

## Results

(none yet)
