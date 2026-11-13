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

`lemnos-isp-proof --checks 1` has a check that rewrites a configuration right after its job is
posted and expects the first configuration's frame. Under the contract that is a write before the
job completes, which rule 1 forbids; the frame is the first configuration's only because the driver
takes the job first. Record here whether it still passes; a failure is not a driver fault, and the
check should move to after completion.

## Results

(none yet)
