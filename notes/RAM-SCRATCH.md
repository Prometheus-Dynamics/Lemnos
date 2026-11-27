# RAM-SCRATCH

The board tests of three crates put their scratch trees in RAM when the gate gives them a RAM
directory (`LEMNOS_TEST_RAM_DIR`, `testing/crates/scratch`, `ram_temp_dir`).

## Why

The tests write their board trees and flush them (`fdatasync`, through the slot store's flush worker)
hundreds of times. In the gate, TMPDIR sits on the disk the gate's own builds write to, and each flush
waits behind those writes. One uboot-fallback cut part (256 boots), alone on one core:

| TMPDIR | wall | user + sys | voluntary switches |
|---|---|---|---|
| tmpfs | 2.15 s | 1.02 + 1.06 s | 12 |
| NVMe btrfs, while a gate built | 385.30 s | 1.32 + 8.75 s | 73 868 |
| NVMe btrfs, a quieter moment | about 10 s | | |

Sampling the threads' wait channel every 0.1 s: the test thread waits on the flush worker (futex), and the
worker is in `btrfs_sync_log`, `folio_wait_bit` and `btrfs_btree_wait_writeback_range` (state D). The same
part in trains: 100.7 s (train 12), 502.1 s (train 13), 150.1 s (train 14). Not CPU starvation and no
product timeout: the flush waits on the disk.

## Before and after, under the same train

Part 1 of the uboot-fallback cut, core 30, nice 10, while train 15 ran its gate:

| round | before (TMPDIR on btrfs) | after (RAM directory) |
|---|---|---|
| 1 (load 75) | 64.13 s, 29 015 voluntary switches | 2.00 s, 11 |
| 2 (load 62) | 4.08 s, 6 842 | 2.24 s, 10 |

The other slow tests of these crates, each alone, tmpfs then btrfs, during a gate's build (load 44-52):
`system-slots` `a_cut_in_the_uboot_count_clear_...` 0.98 / 20.87 s, `a_malformed_signature_is_refused_in_every_row`
0.79 / 18.25 s, `a_refused_boot_count_is_retried_...` 0.06 / 4.89 s, the other fifteen measured 0.07-0.34 / 0.23-1.20 s;
`system-security` `a_cut_at_every_byte_of_the_off_...` 4.97 / 16.17 s. Not moved, because they make no files and
take the same time on both (CPU): the slot store's sweeps (`sweep_uboot_install` 20.35 / 18.86 s,
`power_cut_at_every_sector_write_...` 9.94 / 12.48 s, `a_pointer_cut_at_every_byte_...` 4.46 / 1.84 s).

## What moved

Every test that makes its board through the three helpers: the slot-init board (`boot/tests.rs`, `Board::fresh`,
and the environment device file of `the_device_form_writes_the_environment_where_the_locator_says`), the
system-slots `TempDir` (and the leftover-name test that plants files beside it), and the system-security flow
tests' `Dir`. None asserts what reaches a disk: their power cuts and crashes are faults the fakes inject in the
process (a byte count where a write stops, a step where the flow stops), the same on any filesystem. The
crash test on a real filesystem is `testing/crates/crash-init` (QEMU); it does not use these helpers and stays
as it is. The list below is every test that called `ram_temp_dir` (`LEMNOS_TEST_RAM_LOG`), 353 in all.

## The gate

`scripts/gate.sh` makes the directory under `XDG_RUNTIME_DIR`, checks it is tmpfs with `GATE_RAM_NEED_KIB`
free (default 1 GiB), and fails by name if not; nothing falls back to the disk. It samples the directory's use
each second and prints the peak, fails if anything is left in it when the gate passes, and removes it on every
exit. CI (`GITHUB_ACTIONS`), a machine without `XDG_RUNTIME_DIR`, and a caller that sets the variable itself
are left as before.

### lemnos-slot-init (127)

- `boot::tests::a_bind_that_fails_is_refused_by_name_and_the_boot_completes`
- `boot::tests::a_board_that_states_no_boot_source_is_refused_without_touching_a_slot`
- `boot::tests::a_boot_chain_with_no_source_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::a_boot_disk_listed_before_its_partitions_is_waited_for_until_they_are_there`
- `boot::tests::a_boot_disk_that_never_shows_up_is_refused_after_the_wait_with_what_was_seen`
- `boot::tests::a_boot_disk_that_shows_up_late_is_waited_for_and_the_boot_goes_on`
- `boot::tests::a_boot_disk_without_exactly_one_environment_partition_is_refused_by_name`
- `boot::tests::a_boot_partition_that_wont_mount_still_boots_the_command_lines_slot`
- `boot::tests::a_boot_source_the_table_does_not_list_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::a_boot_source_the_table_does_not_list_is_refused_without_waiting`
- `boot::tests::a_command_line_changed_by_one_argument_does_not_match`
- `boot::tests::a_command_line_with_no_slot_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::a_count_below_the_baseline_after_a_power_loss_resets_the_baseline_to_zero`
- `boot::tests::a_count_picked_uboot_previous_is_a_trial_as_the_pi_previous_is`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_1_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_2_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_3_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_4_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_5_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_6_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_7_of_8`
- `boot::tests::a_cut_at_every_byte_of_the_uboot_fallback_leaves_the_old_or_the_new_environment_part_8_of_8`
- `boot::tests::a_cut_short_image_is_refused`
- `boot::tests::a_data_partition_with_errors_mounts_read_only`
- `boot::tests::a_default_mismatch_on_the_uboot_form_moves_the_default_through_the_environment`
- `boot::tests::a_fallback_trial_gets_the_attempts_the_board_was_given`
- `boot::tests::a_file_record_trial_held_for_a_kernel_gap_is_not_started_until_the_gap_is_gone`
- `boot::tests::a_file_record_trial_is_not_started_when_its_attempt_cannot_be_written`
- `boot::tests::a_firmware_trial_on_a_boot_partition_without_our_records_says_it_runs_unconfirmable`
- `boot::tests::a_first_boot_after_a_flash_at_count_50_boots_normally_and_sets_the_baseline_to_50`
- `boot::tests::a_flashing_mode_the_firmware_refuses_still_restarts_the_old_way`
- `boot::tests::a_foreign_disk_at_a_count_of_eight_writes_nothing_asks_for_no_flash_mode_and_restarts`
- `boot::tests::a_foreign_disk_with_the_same_label_on_another_controller_is_ignored`
- `boot::tests::a_good_boot_resets_the_baseline_so_the_count_climbing_across_good_boots_is_not_flash_mode`
- `boot::tests::a_healthy_board_across_plain_restarts_and_trials_never_reaches_the_flashing_mode`
- `boot::tests::a_kernel_gap_on_a_foreign_disk_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::a_kernel_gap_on_raw_nand_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::a_kernel_without_a_storage_driver_names_it_when_no_data_partition_answers`
- `boot::tests::a_linked_state_dir_is_not_given_away`
- `boot::tests::a_locator_that_is_not_one_is_refused_by_name`
- `boot::tests::a_manifest_for_another_slot_is_refused`
- `boot::tests::a_missing_image_is_refused`
- `boot::tests::an_absent_overlay_marker_is_a_named_failure`
- `boot::tests::an_environment_partition_too_small_for_two_copies_is_refused_by_name`
- `boot::tests::an_erased_or_wrong_size_environment_is_refused_by_name_and_not_written`
- `boot::tests::an_image_that_wont_mount_falls_back`
- `boot::tests::an_owned_boot_gives_the_slots_dirs_to_the_daemons_user`
- `boot::tests::an_unreadable_boot_disk_asks_for_no_flash_mode_past_the_limit`
- `boot::tests::an_untouched_slot_passes_the_every_boot_check`
- `boot::tests::a_pending_tryboot_trial_takes_an_attempt_and_restarts_into_it_before_anything_else`
- `boot::tests::a_pi_count_picked_previous_starts_as_a_trial_and_asks_for_no_flashing_mode`
- `boot::tests::a_real_pending_trial_is_untouched_by_the_loaders_fallback_to_previous`
- `boot::tests::a_refused_count_clear_still_restarts_into_the_trial_and_says_it_is_counted`
- `boot::tests::a_root_hash_that_isnt_the_command_lines_is_refused`
- `boot::tests::a_root_that_cant_start_lemnosd_falls_back_naming_what_is_missing`
- `boot::tests::a_root_with_no_machine_id_file_is_refused_and_nothing_is_created`
- `boot::tests::a_route_a_boot_gives_nothing_away`
- `boot::tests::a_route_a_boot_that_fails_keeps_its_log_off_the_other_systems_state_dir`
- `boot::tests::a_route_a_boot_writes_nothing_under_the_other_systems_state_dir`
- `boot::tests::a_second_start_on_previous_counts_another_failure_of_the_default`
- `boot::tests::a_serial_that_cannot_be_read_is_refused_as_absent`
- `boot::tests::a_slot_built_for_another_kernel_is_refused`
- `boot::tests::a_slot_built_for_this_kernel_boots`
- `boot::tests::a_slot_failure_deep_into_the_count_falls_back_and_never_asks_for_the_flashing_mode`
- `boot::tests::a_slot_not_checked_at_install_still_boots_while_signing_is_not_required`
- `boot::tests::a_slot_that_isnt_the_default_is_a_trial`
- `boot::tests::a_slot_the_old_installer_made_is_refused_by_name_and_the_default_is_repointed`
- `boot::tests::a_slot_whose_overlay_marker_is_live_boots`
- `boot::tests::a_source_the_table_does_not_list_is_refused_without_touching_a_slot`
- `boot::tests::a_stale_overlay_digest_is_a_named_failure`
- `boot::tests::a_torn_trial_record_boots_the_default_and_never_restarts_into_a_trial`
- `boot::tests::a_trial_fallback_on_the_uboot_form_arms_the_trial_in_the_environment`
- `boot::tests::a_trial_in_the_uboot_environment_held_for_a_kernel_gap_is_withdrawn`
- `boot::tests::a_trial_is_told_from_the_firmwares_flag`
- `boot::tests::a_trial_whose_root_cant_start_lemnosd_restarts_into_the_default`
- `boot::tests::a_tryboot_trial_boot_takes_nothing_more`
- `boot::tests::a_tryboot_trial_held_for_a_kernel_gap_gives_back_its_attempt_and_takes_no_more`
- `boot::tests::a_tryboot_trial_with_no_attempts_left_is_removed_and_the_default_boots`
- `boot::tests::a_uboot_fallback_is_refused_by_name_and_the_record_is_left_alone`
- `boot::tests::a_uboot_fallback_names_the_failed_bay_previous_and_a_third_install_never_takes_it`
- `boot::tests::a_uboot_trial_held_for_a_kernel_gap_is_withdrawn_with_its_attempt_given_back`
- `boot::tests::a_verified_boot_clears_the_unverified_start_record`
- `boot::tests::a_verity_failure_is_recorded_and_the_board_falls_back`
- `boot::tests::a_way_back_the_firmware_refuses_lets_the_boot_go_on`
- `boot::tests::below_the_limit_or_without_the_way_back_the_boot_starts_its_slot`
- `boot::tests::boots_that_die_after_this_program_reach_the_flashing_mode_at_the_limit_before_any_slot_starts`
- `boot::tests::default_and_previous_both_bad_stay_and_report`
- `boot::tests::default_boot_files_that_do_not_match_fall_back_to_previous`
- `boot::tests::devtmpfs_is_mounted_before_anything_that_can_need_the_mailbox`
- `boot::tests::e2e::a_trial_whose_kernel_never_boots_comes_back_to_the_default_and_the_count_stays_low`
- `boot::tests::e2e::end_to_end_pi_firmware_form_install_trial_confirm_keep_previous_free`
- `boot::tests::e2e::end_to_end_uboot_form_install_trial_confirm_keep_previous_free`
- `boot::tests::each_missing_kernel_piece_is_named_and_no_slot_is_failed_or_spent`
- `boot::tests::eight_failed_boots_after_the_baseline_reach_the_flashing_mode_and_not_before`
- `boot::tests::grow_state::a_board_file_that_opts_the_grow_out_records_it_and_opens_no_disk`
- `boot::tests::grow_state::a_call_that_never_returns_leaves_its_marker_stops_feeding_and_the_next_boot_refuses_it`
- `boot::tests::no_disk_under_the_named_controller_is_refused_without_touching_a_slot`
- `boot::tests::no_partition_labelled_lemnos_data_waits_and_restarts`
- `boot::tests::no_serial_is_refused_by_name_and_the_boot_completes`
- `boot::tests::no_slot_on_the_command_line_waits_and_restarts`
- `boot::tests::on_nand_a_cut_short_volume_falls_back_through_the_record_volume`
- `boot::tests::on_nand_a_failed_trial_restarts_plainly_and_leaves_the_record`
- `boot::tests::on_nand_a_verity_failure_falls_back_and_the_default_stays`
- `boot::tests::on_nand_the_slot_boots_from_its_volume_through_ubiblock`
- `boot::tests::refusals_at_the_limit_go_to_the_flashing_mode_and_change_no_record`
- `boot::tests::refusals_below_the_limit_restart_as_before`
- `boot::tests::rule_1_a_failed_trial_restarts_plainly_into_the_default`
- `boot::tests::rule_2_falling_back_stops_after_three_in_a_row`
- `boot::tests::rule_3_a_previous_slot_that_failed_gives_way_to_a_good_one`
- `boot::tests::rule_3_only_a_slot_with_boot_files_on_the_boot_partition_is_tried`
- `boot::tests::rule_4_a_board_with_one_slot_waits_and_restarts_into_it`
- `boot::tests::started_on_previous_as_the_loaders_fallback_it_is_a_trial_and_the_default_is_marked`
- `boot::tests::the_data_partition_is_found_by_the_fixed_label_lemnos_data`
- `boot::tests::the_default_slot_boots_through_dm_verity`
- `boot::tests::the_default_started_with_the_property_set_marks_nothing`
- `boot::tests::the_device_form_writes_the_environment_where_the_locator_says`
- `boot::tests::the_machine_id_is_derived_from_the_serial_and_bound_before_switch_root`
- `boot::tests::the_restart_into_a_trial_clears_the_firmware_count_first_where_there_is_one`
- `boot::tests::trial_boot_files_that_do_not_match_fail_the_trial`
- `boot::tests::two_disks_under_the_named_controller_are_refused_without_touching_a_slot`
- `boot::tests::two_lemnos_data_partitions_ask_for_no_flash_mode_past_the_limit`
- `boot::tests::two_partitions_labelled_lemnos_data_are_refused_at_once_by_name`
- `boot::tests::unreadable_trial_records_with_no_firmware_trial_boot_the_default`
- `boot::tests::without_the_loaders_property_nothing_is_recorded_against_the_default`
- `boot::tests::with_require_signed_an_unverified_default_still_boots_and_is_reported`
- `boot::tests::with_require_signed_a_slot_whose_signature_did_not_verify_is_not_started`
- `boot::tests::with_require_signed_a_verified_slot_boots`

### lemnos-driver-system-slots (193)

- `boot_guard_tests::every_boot_file_kind_reads_raw_through_a_fragmented_fat_image`
- `boot::tests::a_config_txt_without_the_select_lines_refuses_slot_setup_and_is_never_edited`
- `boot::tests::a_config_txt_with_the_old_mask_filter_is_degraded_and_refuses_a_trial`
- `boot::tests::a_tryboot_record_with_no_known_device_or_no_boot_volume_refuses`
- `boot::tests::the_file_record_keeps_both_lines`
- `boot::tests::the_tryboot_record_writes_the_pointers_the_firmware_reads_one_sector_each`
- `boot::tests::the_tryboot_trial_has_its_attempts_beside_it_and_goes_with_it`
- `offline_tests::a_bay_of_another_build_of_the_same_slot_is_refused_by_name_and_nothing_is_left`
- `offline_tests::a_boot_image_whose_config_txt_has_no_select_lines_is_refused_before_anything_is_written`
- `offline_tests::a_boot_image_with_another_bay_layout_is_refused`
- `offline_tests::a_boot_image_with_no_bay_for_the_slot_is_refused`
- `offline_tests::a_bundle_for_another_board_is_refused`
- `offline_tests::a_bundle_with_a_signature_that_does_not_verify_is_refused_and_nothing_is_left`
- `offline_tests::a_data_size_that_fits_the_slot_and_its_records_is_written_with_no_reserve`
- `offline_tests::a_data_size_too_small_for_the_tree_is_refused_and_nothing_is_left`
- `offline_tests::a_default_that_would_change_to_another_bay_is_refused`
- `offline_tests::a_file_that_is_not_a_boot_partition_fails_in_the_writer_and_in_the_live_record_alike`
- `offline_tests::an_existing_output_directory_is_refused_and_left_alone`
- `offline_tests::a_tree_verified_against_a_bay_of_another_build_is_refused_by_name`
- `offline_tests::files_are_0644_and_directories_0755_whatever_the_umask`
- `offline_tests::one_byte_short_of_the_slot_and_its_records_is_refused_with_both_sizes`
- `offline_tests::the_offline_tree_equals_a_live_install_trial_and_confirm_and_reads_as_default`
- `offline_tests::the_same_inputs_give_the_same_bytes_and_another_time_changes_only_the_stamps`
- `route_a_tests::our_own_disk_still_writes_at_start_and_on_confirm`
- `route_a_tests::records_that_cannot_be_judged_still_clear_the_count_once_up_and_write_nothing`
- `route_a_tests::route_a_writes_nothing_at_start_on_ticks_or_by_any_call`
- `tests::a_bad_cursor_is_refused_and_a_buffer_too_small_for_a_slot_is_too_small`
- `tests::a_board_that_requires_signing_and_trusts_no_key_installs_nothing`
- `tests::a_board_without_a_uboot_env_partition_refuses_a_trial_before_anything_is_written`
- `tests::a_board_with_require_signed_on_and_no_key_list_refuses_every_bundle`
- `tests::a_boot_file_that_differs_from_the_manifest_digest_is_refused`
- `tests::a_boot_file_the_boot_partition_does_not_store_is_refused_before_any_write`
- `tests::a_boot_good_that_did_not_finish_leaves_no_mark_and_the_next_daemon_does_it`
- `tests::a_boot_writes_one_history_entry_even_when_the_driver_starts_again_within_it`
- `tests::a_built_in_device_that_didnt_bind_here_blocks_the_confirm`
- `tests::a_built_in_device_that_is_gone_blocks_the_confirm_and_is_named`
- `tests::a_built_in_device_that_worked_before_and_is_absent_now_blocks_the_confirm`
- `tests::a_built_in_device_that_worked_before_and_is_failed_now_blocks_the_confirm_and_is_named`
- `tests::a_bundle_for_the_running_or_default_slot_is_refused_before_its_image_comes`
- `tests::a_bundle_made_by_the_writer_installs_on_both_record_forms`
- `tests::a_bundle_rebuilt_from_the_kept_manifest_and_slot_sig_installs_with_the_switch_on`
- `tests::a_bundle_whose_command_line_lacks_its_own_slot_or_root_hash_is_refused_by_name`
- `tests::a_cancel_after_the_payloads_are_in_frees_their_bay`
- `tests::a_cancel_by_the_starter_and_one_by_its_connection_ending_are_told_apart`
- `tests::a_cancelled_install_ends_as_cancelled_and_leaves_nothing`
- `tests::a_cancelled_install_shows_failed`
- `tests::accept_says_whether_a_bundle_fits_with_the_reserve`
- `tests::a_config_txt_without_the_select_lines_leaves_the_driver_running_degraded_and_listed`
- `tests::a_confirmed_trial_becomes_the_default_and_old_slots_past_keep_go`
- `tests::a_confirm_on_a_firmware_board_keeps_the_count_when_the_boot_files_cannot_be_read`
- `tests::a_cut_in_the_uboot_count_clear_leaves_the_real_environment_whole_and_the_next_good_boot_finishes_it`
- `tests::a_deadline_restart_retries_the_reason_with_capped_backoff_before_it_restarts`
- `tests::a_failed_state_write_shows_in_health_and_info_until_one_succeeds`
- `tests::a_failure_in_the_work_is_recorded_as_failed_with_its_reason`
- `tests::a_fallen_back_trial_is_recorded_in_the_boot_history_with_its_reason`
- `tests::a_file_in_incoming_installs_as_a_job_and_is_deleted_after`
- `tests::a_file_removed_mid_install_still_installs_from_what_was_opened`
- `tests::a_file_that_fails_or_is_cancelled_is_deleted_too`
- `tests::a_finished_boot_good_is_marked_and_a_restarted_daemon_of_the_same_boot_does_not_do_it_again`
- `tests::a_gpt_environment_on_a_boot_disk_with_a_proper_partition_reports_trials_available`
- `tests::a_gpt_environment_whose_boot_partition_is_not_fat_refuses_a_trial`
- `tests::a_gpt_environment_whose_disk_is_not_known_refuses_a_trial`
- `tests::a_gpt_environment_with_a_fat_uboot_env_file_refuses_a_trial`
- `tests::a_health_source_not_good_by_the_deadline_restarts_the_board_and_names_it`
- `tests::a_healthy_trial_confirms_by_itself_and_is_recorded`
- `tests::a_hotplug_device_that_is_gone_is_not_a_regression`
- `tests::a_hotplug_device_that_worked_before_and_is_absent_in_the_trial_still_confirms`
- `tests::a_hot_pluggable_device_failed_while_bound_still_blocks_the_confirm`
- `tests::a_hot_pluggable_device_that_is_unbound_and_failed_is_not_a_regression`
- `tests::a_leftover_file_at_the_next_scratch_name_does_not_stop_a_test`
- `tests::a_listed_member_that_is_missing_is_refused`
- `tests::a_malformed_signature_is_refused_in_every_row`
- `tests::a_malformed_view_reads_on_on_a_board_with_no_trust_root_and_info_says_so`
- `tests::a_missing_marker_refuses_a_trial_as_a_loader_that_is_not_updated`
- `tests::a_missing_mount_is_said_in_words_and_the_boot_goes_on`
- `tests::an_empty_pstore_is_recorded_as_no_record`
- `tests::a_new_attempt_replaces_the_record_when_it_starts`
- `tests::an_explicit_confirm_during_the_wait_is_recorded_with_its_caller`
- `tests::an_image_or_boot_file_that_isnt_what_the_manifest_says_is_refused`
- `tests::an_install_a_restart_cut_short_is_known_after_it_and_nothing_else_is`
- `tests::an_installed_slot_is_recorded_with_its_bytes_and_time_for_a_second_session`
- `tests::an_install_from_a_file_is_recorded_as_a_streamed_one_is`
- `tests::an_install_is_written_and_its_reply_says_the_loader_will_not_start_it`
- `tests::an_install_the_service_restart_cut_short_is_cleared_and_reported`
- `tests::an_unlisted_boot_file_is_refused_before_it_is_written`
- `tests::an_unreadable_environment_refuses_a_trial_before_anything_is_written`
- `tests::an_unsigned_install_keeps_no_slot_sig_and_clears_one_an_earlier_install_left`
- `tests::an_unwritable_boot_file_store_refuses_an_install_before_any_byte`
- `tests::an_unwritable_state_dir_refuses_a_switch_before_any_boot_record_is_written`
- `tests::a_power_cut_on_each_of_three_boots_is_still_tried_and_the_fourth_confirms`
- `tests::a_reason_past_the_bound_is_cut_with_the_marker_and_the_state_keeps_it_whole`
- `tests::a_reason_the_state_never_held_is_not_kept_on_the_boot_partition`
- `tests::a_record_is_stored_for_the_boot_after_the_crash_and_only_then_cleared`
- `tests::a_record_that_fits_the_bound_is_kept_whole_with_no_cut_line`
- `tests::a_refusal_before_the_install_starts_is_recorded_too`
- `tests::a_refusal_is_recorded_with_the_reason_the_starter_was_given`
- `tests::a_refusal_the_finish_finds_comes_back_by_name`
- `tests::a_refused_boot_count_is_retried_from_the_tick_and_said_in_health_and_info`
- `tests::a_refused_bundle_writes_nothing_and_the_root_image_is_not_consumed`
- `tests::a_refused_install_names_the_ungrown_partition_while_the_grow_is_failing`
- `tests::a_refused_removal_is_stored_once_and_not_twice_on_the_next_boot`
- `tests::a_repeated_member_or_an_unknown_one_is_refused_by_name`
- `tests::a_restart_does_not_give_back_the_trial_attempt_while_the_loader_cannot_read_the_record`
- `tests::a_role_file_the_manifest_leaves_out_is_refused_by_name`
- `tests::a_root_image_that_differs_from_the_manifest_digest_is_refused`
- `tests::a_session_that_did_not_start_it_sees_a_running_install_as_the_job_shows_it`
- `tests::a_signature_over_the_bare_manifest_is_refused_as_a_bad_signature_under_a_known_key`
- `tests::a_signed_bundle_streams_in_and_is_put_where_the_boot_chain_looks`
- `tests::a_signed_install_keeps_its_slot_sig_byte_for_byte_and_remove_deletes_it`
- `tests::a_signed_slot_with_no_kept_slot_sig_is_named_as_installed_before_signatures_were_kept`
- `tests::a_single_slot_board_has_no_trial_and_writes_its_baseline_once_it_is_up`
- `tests::a_slot_built_for_this_board_installs_and_one_for_another_is_refused`
- `tests::a_slot_for_another_medium_is_refused_by_name_at_install_and_trial`
- `tests::a_slot_id_of_the_wrong_form_is_refused_at_the_manifest`
- `tests::a_slot_installed_between_pages_is_neither_lost_nor_repeated`
- `tests::a_slot_on_disk_for_another_board_is_listed_but_never_booted`
- `tests::a_slot_recorded_under_the_old_labels_is_listed_as_before`
- `tests::a_slot_state_that_cant_be_written_does_not_confirm`
- `tests::a_slot_whose_signature_did_not_verify_is_not_tried_once_signing_is_required`
- `tests::a_store_that_fails_keeps_the_record_on_pstore`
- `tests::a_streamed_install_bigger_than_a_tick_finishes_through_its_job`
- `tests::a_switch_is_refused_before_anything_is_written_when_the_loader_cannot_read_the_record`
- `tests::a_trial_is_judged_after_the_board_loses_its_previous_slot`
- `tests::a_trial_is_not_armed_when_the_restart_it_needs_is_refused`
- `tests::a_trial_on_a_single_slot_board_falls_back_to_the_one_slot`
- `tests::a_trial_that_keeps_the_default_leaves_it`
- `tests::a_trial_whose_accepted_restart_never_stops_is_disarmed_by_the_tick`
- `tests::a_trial_whose_restart_fails_to_start_is_disarmed`
- `tests::a_trial_with_a_tcp_source_confirms_only_after_the_listener_appears`
- `tests::a_uboot_env_partition_and_the_marker_make_trials_available`
- `tests::a_verify_slower_than_the_call_deadline_neither_overruns_a_tick_nor_loops`
- `tests::confirming_a_trial_that_would_become_the_default_is_refused_when_the_loader_cannot_read_the_record`
- `tests::housekeeping_keeps_three_by_default`
- `tests::incoming_is_emptied_and_group_writable_when_the_driver_starts`
- `tests::info_names_the_held_keys_by_id_and_owner`
- `tests::installing_an_installed_slot_again_replaces_it_in_place_with_fresh_metadata`
- `tests::keep_old_pinned_and_keep_zero_hold_slots_back_from_housekeeping`
- `tests::on_a_slow_store_each_tick_of_a_boot_file_copy_stays_inside_its_share_of_the_deadline`
- `tests::on_a_slow_store_the_payload_write_into_a_bay_stays_inside_its_share_of_the_deadline`
- `tests::once_the_reason_can_be_written_the_deadline_restart_goes_and_nothing_is_reported`
- `tests::one_bundle_installs_on_a_board_that_requires_signing_and_on_one_that_does_not`
- `tests::only_a_plain_file_named_by_one_component_installs`
- `tests::only_the_starter_writes_and_only_where_the_install_is`
- `tests::on_nand_a_device_with_no_room_left_at_the_volume_refuses_the_install_by_name`
- `tests::on_nand_an_install_writes_its_image_into_a_volume_and_renames_it_into_place`
- `tests::on_nand_a_refused_or_cancelled_install_leaves_no_work_volume`
- `tests::on_nand_a_restart_mid_install_removes_the_work_volume`
- `tests::on_nand_remove_takes_the_volume_and_gives_its_room_back`
- `tests::on_nand_room_is_the_ubi_devices`
- `tests::on_nand_the_uboot_record_is_a_volume_replaced_whole`
- `tests::remove_never_takes_a_slot_the_board_needs`
- `tests::set_default_wants_a_slot_that_booted_well`
- `tests::slot_json_must_be_the_first_member`
- `tests::slot_sig_must_be_the_second_member`
- `tests::the_boot_history_carries_the_reset_reason_the_board_reports_and_the_request_before_it`
- `tests::the_boot_history_keeps_only_the_newest_boots_kept`
- `tests::the_boot_history_records_a_trial_as_on_trial_and_resolves_its_entry_once`
- `tests::the_boots_contract_method_writes_the_history_or_refuses_a_buffer_that_is_too_small`
- `tests::the_call_that_takes_the_last_byte_returns_before_the_finish_runs`
- `tests::the_file_lemnos_ctl_bundle_wrote_installs_on_the_board_with_the_signature_enforced`
- `tests::the_five_role_files_install_and_the_slot_passes_its_boot_check`
- `tests::the_indicator_shows_each_phase_of_an_install_and_only_changes`
- `tests::the_info_text_carries_the_crash_note_only_for_a_declared_log`
- `tests::the_list_has_the_trial_and_the_resolution_as_they_stand`
- `tests::the_list_says_what_is_installed_and_which_is_which`
- `tests::the_list_shows_each_slots_build_and_medium_or_not_recorded`
- `tests::the_loader_script_reads_only_what_the_writers_wrote_and_its_previous_step_can_run`
- `tests::the_paged_list_walks_every_slot_once_in_the_order_of_the_list`
- `tests::the_policy_with_require_signed_off_accepts_every_row_and_says_how_it_was_checked`
- `tests::the_previous_slot_may_be_removed_and_a_trial_still_falls_back`
- `tests::the_reason_the_trial_slot_recorded_is_the_one_that_is_given`
- `tests::the_refusal_of_a_stored_file_comes_before_the_first_payload_byte`
- `tests::the_slow_boot_file_look_runs_off_the_tick_and_its_answer_is_taken_by_a_later_tick`
- `tests::the_uboot_arrival_on_previous_records_an_episode_in_the_real_environment`
- `tests::the_uboot_arrival_records_its_episode_and_refuses_the_demotion_when_the_loader_cannot_read_the_record`
- `tests::the_uboot_good_boot_clears_the_count_and_the_episodes_in_the_real_environment`
- `tests::the_uboot_good_boot_clears_the_count_and_the_episodes_when_the_loader_cannot_read_the_record`
- `tests::three_boots_that_never_reach_the_daemon_fall_back_with_the_reason_and_it_is_recorded_on_the_next_boot`
- `tests::through_a_host_an_install_is_a_job_and_a_trial_restarts_with_tryboot`
- `tests::with_auto_confirm_off_nothing_confirms_or_restarts_by_itself`
- `tests::with_no_attempt_ever_the_field_says_none`
- `tests::with_no_crash_log_declared_the_history_is_what_it_was`
- `tests::without_require_signed_any_bundle_installs_and_the_list_says_how_it_was_signed`
- `tests::with_reboot_at_deadline_off_the_reason_is_kept_and_the_board_is_not_restarted`
- `tests::with_require_signed_an_unverified_default_is_reported_and_the_driver_keeps_running`
- `tests::with_require_signed_off_a_bad_signature_under_a_known_key_is_refused_before_anything_is_installed`
- `tests::with_require_signed_on_the_table_refuses_three_rows_and_takes_a_verified_bundle`
- `tests::with_the_switch_on_a_bundle_not_signed_by_a_trusted_key_is_refused_and_leaves_nothing`
- `tests::with_the_uboot_record_an_install_writes_the_slots_extlinux_and_a_trial_arms_it`
- `uboot::grant_tests::a_fat_environment_on_the_boot_partition_makes_the_hand_over_read_only_and_the_write_is_refused_by_name`
- `uboot::grant_tests::the_daemon_writes_the_environment_through_the_partition_the_root_phase_handed_over`
- `uboot::tests::the_environment_record_refuses_by_name_when_the_disk_or_the_environment_is_not_there`
- `uboot::tests::the_file_form_is_refused_by_name_and_the_environment_record_round_trips`

### lemnos-driver-system-security (33)

- `flow::tests::a_bad_signature_by_a_held_key_does_not_verify`
- `flow::tests::a_change_with_every_step_done_leaves_the_mark_the_record_and_the_view_agreeing`
- `flow::tests::a_change_writes_the_record_then_renders_the_view_at_once`
- `flow::tests::a_crash_at_each_step_of_a_change_to_off_never_leaves_the_mark_weaker_than_the_record`
- `flow::tests::a_crash_at_each_step_of_a_change_to_on_never_leaves_the_mark_weaker_than_the_record`
- `flow::tests::a_cut_after_the_record_and_before_the_view_leaves_the_view_pending_and_the_authorization_spent`
- `flow::tests::a_cut_at_every_byte_of_the_off_leaves_the_same_end_state_or_a_spent_authorization`
- `flow::tests::a_failed_render_leaves_the_view_pending_and_the_retry_brings_it_current`
- `flow::tests::a_key_the_board_does_not_hold_is_named`
- `flow::tests::a_malformed_authorization_is_refused_by_name`
- `flow::tests::a_mark_that_cannot_be_cleared_is_reported_and_stays_stricter_than_the_record`
- `flow::tests::a_mark_that_cannot_be_written_stops_a_change_to_on_before_the_record`
- `flow::tests::an_authorization_for_a_future_generation_is_not_named_as_past`
- `flow::tests::an_authorization_given_with_on_is_ignored_and_says_so`
- `flow::tests::an_authorization_is_spent_by_the_change_it_allowed`
- `flow::tests::an_authorization_made_for_another_board_does_not_verify_here`
- `flow::tests::an_authorization_made_for_on_is_named_as_another_state`
- `flow::tests::an_authorized_off_cut_before_each_step_ends_consistently_and_the_authorization_follows_the_record`
- `flow::tests::a_record_that_cannot_be_written_refuses_and_leaves_the_view_alone`
- `flow::tests::a_record_written_before_restart_is_the_truth_at_the_next_open`
- `flow::tests::a_refused_change_writes_nothing_and_says_why`
- `flow::tests::a_slot_bundle_signature_over_the_same_bytes_is_not_an_authorization`
- `flow::tests::a_valid_authorization_turns_it_off_and_moves_the_generation_once`
- `flow::tests::off_with_a_trust_root_and_no_authorization_is_refused_by_name`
- `flow::tests::off_with_no_trust_root_needs_no_authorization_and_ignores_one`
- `flow::tests::open_reads_absent_as_off_and_corrupt_as_on_reported`
- `flow::tests::request_off_names_the_message_an_authorization_signs`
- `flow::tests::request_off_with_no_trust_root_says_off_needs_none`
- `flow::tests::state_names_the_serial_the_trust_root_and_the_keys_held`
- `flow::tests::state_says_what_the_readers_read_when_the_view_is_malformed`
- `flow::tests::the_documented_vector_is_the_message_and_the_record`
- `flow::tests::the_trust_root_is_a_key_line_with_an_owner`
- `flow::tests::with_no_serial_a_trust_root_cannot_be_named_so_off_is_refused`

