# J4b board run: the landing frame on the reference board

Status: untested on board. Nothing here was run on hardware. The landing rule and the free-running
simulator are checked in the simulator only (`testing/crates/camera-proof`, `post_values_at` tests).

Run in a local session after this PR merges, on the CM5 (Raze #2), with an OV9782 on the board
linked to its CSI-2 receiver (`rp1-csi2`) in the board file. Take the board lock named in `AGENTS.md`,
`/run/lock/board-gpu.lock`. Build on NVMe or SSD, never on the Github HDD. Report CM5 timings only.

## Checks

1. **Frame starts count every frame.** Start the receiver and sensor, keep the `frames` stream
   drained, and read `camera.receiver` `frame_starts` once a second. Pass: `count` rises by the
   frame rate each second (about 120 at 1280x400 RAW10 on the 120 fps mode, 60 at the full
   frame), `latest_ns` is a CLOCK_MONOTONIC time within one frame of the read, and the
   sequence numbers of the published frames run from 0 with no gaps, one per count.
2. **A post names the frame its exposure shows on.** With the scene lit and the exposure at a
   short value, post a step to a long exposure with `post_values_at` and note the answered frame
   `N`. Read the frames stream and take each frame's mean brightness. Pass: the first frame whose
   mean rises above the short-exposure level is frame `N` (within the noise of the scene), for
   each of 20 steps made at random times in the frame.
3. **A straddling post lands one frame later.** Post the same step with the write time forced
   across a frame start (post at the end of a frame, so the group's launch falls in the next
   frame). Pass: `post_values_at` answers `N + 1` for each such post, and the brightness step
   lands on `N + 1`, as the landing rule in `contracts/camera-sensor` says.
4. **Refusals.** With the receiver stopped, `post_values_at` answers `BUSY` and writes nothing: the
   sensor's register trace (the bus log) has no group write for the refused post.
5. **Embedded data.** Not applicable to the OV9782 (`SensorInfo::embedded_data` is `None`). No
   check is needed until a sensor with embedded data is on a board.

## Receivers that answer NOT_SUPPORTED

In this PR every receiver answers `frame_starts` with `NOT_SUPPORTED`: `rp1-csi2` (its kernel module
does not yet expose the frame-start count), `rkcif`, `sunxi-vin` and `k230-capture`. On those boards
`post_values_at` answers `NOT_SUPPORTED` and writes nothing, so checks 1 to 4 cannot run until a
receiver serves the count. The next PR wires `rp1-csi2` through the module's uAPI.
