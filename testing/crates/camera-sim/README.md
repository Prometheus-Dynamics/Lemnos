# lemnos-camera-sim

The camera chain on a PC: the real `lemnosd` service and the real sensor, power switch, receiver and
PiSP drivers over simulated buses. See the crate's documentation (`cargo doc`) and the testing guide's
section "The camera simulator" (`docs/testing.md`) for what is simulated and how the paces work.

Two ways in:

- `Sim::builder()` (in process): an instance with its own service, socket, clock, sensor, receiver
  switches and frame log. Two instances in one test process share no state.
- `offboard-daemon --pace real|lockstep`: the process's own chain, for the off-board pass. `lockstep`
  (the default) is the event-driven clock; `real` paces the frames on the sensor's frame period.

## The consumer's asserts, and the Lemnos test that pins each

The consumer's list of what it needs from this crate is kept with the work plan, not here. Each row
is one claim the consumer makes, and the test that makes it fail if the crate stops making it. Tests
are in `tests/instance.rs`.

| The consumer's assert or need | Lemnos test |
|---|---|
| Counters: each window interval within one line of the frame length, at least three windows | `paced_frames_fall_on_the_sensor_period_from_its_start_and_are_stamped_on_the_grid`, `the_frame_length_a_test_sets_moves_the_period_and_the_stamps_follow_it` |
| the period is the sensor's frame length times its line time, not the link's width x height / pixel rate | `the_period_is_the_sensors_frame_length_times_its_line_time` |
| a due frame with no free buffer is dropped and counted, never blocked or queued past the ring | `a_due_frame_with_no_free_buffer_is_dropped_and_counted_and_the_stream_continues` |
| all buffers held: frame dropped and counted, stream continues | `a_due_frame_with_no_free_buffer_is_dropped_and_counted_and_the_stream_continues` |
| stamps: interval within one line of the frame length; `timestamp_ns > 0` | `paced_frames_fall_on_the_sensor_period_from_its_start_and_are_stamped_on_the_grid`, `a_real_paced_stream_stamps_on_its_grid_and_counts_what_it_drops_in_the_sequence` (the host's clock) |
| reader gaps at most 250 ms over at least four seconds | `a_paced_stream_over_four_seconds_of_its_clock_has_no_gap_over_one_period` (the injected clock: how fast a loaded host keeps up is a measurement, not a gate test) |
| paced content: a reader's bytes are the producer's for the same frame id | `paced_frames_carry_the_bytes_of_their_identity` |
| per-instance state: no shared clock or names between two instances in one process | `two_instances_in_one_process_share_no_state` |
| lock-step: `next` advances exactly one frame, in order | `next_advances_exactly_one_frame_in_order` |
| lock-step: a control posted for frame k lands on exactly frame k | `a_control_posted_for_frame_k_lands_on_exactly_frame_k` |
| lock-step: a late control is refused `Late`; a bad value is refused `Value` | `a_late_control_is_refused_late_and_a_bad_value_is_refused_by_value` |
| sensor `Failed`: retried, stays bound, stream stays attached | `a_sensor_failure_keeps_the_stream_attached_and_is_cleared` |
| sensor `Gone`: bound again, the fault spent once seen | `a_gone_sensor_is_spent_once_the_host_has_seen_it` |
| receiver start refused N times with `IO` | `a_refused_receiver_start_is_refused_with_io_the_number_of_times_asked` |
| receiver silent: no frames, no fault, first unmade sequence recorded | `a_silent_receiver_makes_no_frames_and_no_fault_and_records_the_first_unmade_frame` |
| receiver gone for good: `Health::Gone`, the failure rule applies | `a_gone_receiver_reports_gone_for_good` |
| receiver gone, board `auto_off_after`: stream ends device-failed | `a_gone_receiver_with_auto_off_ends_its_stream_device_failed` |
| consumer broke the rules: the next tick ends the consumer | `a_consumer_fault_ends_the_consumer_on_the_next_tick` |
| frame mean is black level plus signal, times exposure and gain, within 1.0 | `exposure_and_gain_scale_the_light_and_the_black_level_is_64` |
| dim is half of bright, within 2.0 | `exposure_and_gain_scale_the_light_and_the_black_level_is_64` |
| still frame: black level 64 | `exposure_and_gain_scale_the_light_and_the_black_level_is_64` |
| static ramp with bands every eight rows | `the_pattern_is_a_static_ramp_with_bands_every_eight_rows` |
| replay: `frames[n % len]` loops | `replay_plays_its_frames_in_turn_and_loops` |
| buffers: dma-heap or memfd, selectable; frames equal the reference row for row | `memfd_frames_equal_the_reference_row_for_row_and_dma_heap_is_used_or_refused` |

## Not carried over

- The PiSP blocks are not in an instance. The driver's simulated-block hook is installed once per
  process, so a PiSP chain runs on the process's own chain only.
- The consumer's memory helper is not moved: the buffers are `Memory::DmaHeap` or `Memory::Memfd`.
- The frame lease a consumer holds is the client's `HeldFrame`.
- The job-ISP double stays with the consumer.
