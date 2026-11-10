/*
 * Generates the layout assertions in drivers/vip9000/src/layout.rs from the vendor kernel's
 * vipcore headers. Not part of the crate build: run ./run.sh <vip2 dir> and it diffs the result
 * against the file.
 *
 * The headers are inc/vip_drv_share.h, inc/vip_lite_config.h, inc/vip_lite_common.h and
 * os/linux/vip_drv_os.h of drivers/npu/aw_nna_vip/vip2/ in the Radxa allwinner-bsp tree at
 * a842bb8dc74608ba13975c97c31b9518f1976c01, built with the options the module's Kbuild sets
 * (see run.sh). Every field has a fixed width, so the numbers hold on aarch64 and x86-64 alike.
 */
#include <stddef.h>
#include <stdio.h>
#include <vip_drv_os.h>

/* Rust type, C type. */
#define SIZE(r, c) printf("    assert!(size_of::<%s>() == %zu);\n", #r, sizeof(c))
/* Rust type and field, C type and member. */
#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))
#define NUM(r, c) printf("    assert!(%s == %#llx);\n", #r, (unsigned long long)(c))

int main(void)
{
	printf("// Structures (vip_drv_os.h, vip_drv_share.h).\n");
	printf("const _: () = {\n");
	SIZE(IoctlData, struct ioctl_data);
	OFF(IoctlData, command, struct ioctl_data, command);
	OFF(IoctlData, error, struct ioctl_data, error);
	OFF(IoctlData, bytes, struct ioctl_data, bytes);
	OFF(IoctlData, buffer, struct ioctl_data, buffer);

	SIZE(Initialize, vipdrv_initialize_t);
	OFF(Initialize, video_mem_size, vipdrv_initialize_t, video_mem_size);
	OFF(Initialize, version, vipdrv_initialize_t, version);

	SIZE(Allocation, vipdrv_allocation_t);
	OFF(Allocation, logical, vipdrv_allocation_t, logical);
	OFF(Allocation, mem_id, vipdrv_allocation_t, mem_id);
	OFF(Allocation, physical, vipdrv_allocation_t, physical);
	OFF(Allocation, size, vipdrv_allocation_t, size);
	OFF(Allocation, align, vipdrv_allocation_t, align);
	OFF(Allocation, alloc_flag, vipdrv_allocation_t, alloc_flag);
	OFF(Allocation, device_vis, vipdrv_allocation_t, device_vis);
	OFF(Allocation, specified, vipdrv_allocation_t, specified);

	SIZE(Deallocation, vipdrv_deallocation_t);
	OFF(Deallocation, mem_id, vipdrv_deallocation_t, mem_id);

	SIZE(WrapUserFd, vipdrv_wrap_user_fd_t);
	OFF(WrapUserFd, fd, vipdrv_wrap_user_fd_t, fd);
	OFF(WrapUserFd, memory_type, vipdrv_wrap_user_fd_t, memory_type);
	OFF(WrapUserFd, size, vipdrv_wrap_user_fd_t, size);
	OFF(WrapUserFd, device_vis, vipdrv_wrap_user_fd_t, device_vis);
	OFF(WrapUserFd, virtual_addr, vipdrv_wrap_user_fd_t, virtual_addr);
	OFF(WrapUserFd, mem_id, vipdrv_wrap_user_fd_t, mem_id);
	OFF(WrapUserFd, logical, vipdrv_wrap_user_fd_t, logical);

	SIZE(Reg, vipdrv_reg_t);
	OFF(Reg, dev_index, vipdrv_reg_t, dev_index);
	OFF(Reg, core_index, vipdrv_reg_t, core_index);
	OFF(Reg, reg, vipdrv_reg_t, reg);
	OFF(Reg, data, vipdrv_reg_t, data);

	SIZE(CreateTask, vipdrv_create_task_param_t);
	OFF(CreateTask, task_id, vipdrv_create_task_param_t, task_id);
	OFF(CreateTask, task_name, vipdrv_create_task_param_t, task_name);
	OFF(CreateTask, subtask_count, vipdrv_create_task_param_t, subtask_count);

	SIZE(DestroyTask, vipdrv_destroy_task_param_t);
	OFF(DestroyTask, task_id, vipdrv_destroy_task_param_t, task_id);

	SIZE(SetTaskProperty, vipdrv_set_task_property_t);
	OFF(SetTaskProperty, task_id, vipdrv_set_task_property_t, task_id);
	OFF(SetTaskProperty, kind, vipdrv_set_task_property_t, type);
	OFF(SetTaskProperty, index, vipdrv_set_task_property_t, index);
	OFF(SetTaskProperty, patch_type, vipdrv_set_task_property_t, patch_type);
	OFF(SetTaskProperty, param, vipdrv_set_task_property_t, param);

	SIZE(SubmitTask, vipdrv_submit_task_param_t);
	OFF(SubmitTask, task_id, vipdrv_submit_task_param_t, task_id);
	OFF(SubmitTask, subtask_index, vipdrv_submit_task_param_t, subtask_index);
	OFF(SubmitTask, subtask_cnt, vipdrv_submit_task_param_t, subtask_cnt);
	OFF(SubmitTask, device_index, vipdrv_submit_task_param_t, device_index);
	OFF(SubmitTask, core_index, vipdrv_submit_task_param_t, core_index);
	OFF(SubmitTask, core_cnt, vipdrv_submit_task_param_t, core_cnt);
	OFF(SubmitTask, priority, vipdrv_submit_task_param_t, priority);
	OFF(SubmitTask, time_out, vipdrv_submit_task_param_t, time_out);
	OFF(SubmitTask, need_schedule, vipdrv_submit_task_param_t, need_schedule);

	SIZE(WaitTask, vipdrv_wait_task_param_t);
	OFF(WaitTask, task_id, vipdrv_wait_task_param_t, task_id);
	OFF(WaitTask, subtask_index, vipdrv_wait_task_param_t, subtask_index);
	SIZE(WaitTask, vipdrv_cancel_task_param_t);
	printf("};\n\n");

	printf("// Numbers (vip_drv_os.h, vip_drv_share.h).\n");
	printf("const _: () = {\n");
	NUM(VIPDRV_IOCTL, VIPDRV_IOCTL);
	NUM(MAX_CMD_SIZE, vipdMAX_CMD_SIZE);
	NUM(APPEND_COMMAND_SIZE, APPEND_COMMAND_SIZE);
	NUM(TASK_NAME_LENGTH, TASK_NAME_LENGTH);
	NUM(Command::ReadReg as u32, VIPDRV_INTRFACE_READ_REG);
	NUM(Command::WriteReg as u32, VIPDRV_INTRFACE_WRITE_REG);
	NUM(Command::WaitTask as u32, VIPDRV_INTRFACE_WAIT_TASK);
	NUM(Command::SubmitTask as u32, VIPDRV_INTRFACE_SUBMIT_TASK);
	NUM(Command::Allocation as u32, VIPDRV_INTRFACE_ALLOCATION);
	NUM(Command::Deallocation as u32, VIPDRV_INTRFACE_DEALLOCATION);
	NUM(Command::WrapUserMemory as u32, VIPDRV_INTRFACE_WRAP_USER_MEMORY);
	NUM(Command::WrapUserPhysical as u32, VIPDRV_INTRFACE_WRAP_USER_PHYSICAL);
	NUM(Command::WrapUserFd as u32, VIPDRV_INTRFACE_WRAP_USER_FD);
	NUM(Command::UnwrapUserFd as u32, VIPDRV_INTRFACE_UNWRAP_USER_FD);
	NUM(Command::QueryMmuInfo as u32, VIPDRV_INTRFACE_QUERY_MMU_INFO);
	NUM(Command::MapUserLogical as u32, VIPDRV_INTRFACE_MAP_USER_LOGICAL);
	NUM(Command::CancelTask as u32, VIPDRV_INTRFACE_CANCEL_TASK);
	NUM(Command::CreateTask as u32, VIPDRV_INTRFACE_CREATE_TASK);
	NUM(Command::SetTaskProperty as u32, VIPDRV_INTRFACE_SET_TASK_PROPERTY);
	NUM(Command::DestroyTask as u32, VIPDRV_INTRFACE_DESTROY_TASK);
	NUM(Command::ALL.len(), VIPDRV_INTRFACE_MAX);
	NUM(alloc_flag::CONTIGUOUS, VIPDRV_VIDEO_MEM_ALLOC_CONTIGUOUS);
	NUM(alloc_flag::NO_MMU_PAGE, VIPDRV_VIDEO_MEM_ALLOC_NO_MMU_PAGE);
	NUM(alloc_flag::MAP_USER, VIPDRV_VIDEO_MEM_ALLOC_MAP_USER);
	NUM(alloc_flag::MMU_PAGE_MEM, VIPDRV_VIDEO_MEM_ALLOC_MMU_PAGE_MEM);
	NUM(task_cmd::SUBTASK, VIPDRV_TASK_CMD_SUBTASK);
	NUM(task_cmd::PRELOAD, VIPDRV_TASK_CMD_PRELOAD);
	NUM(patch::NONE, VIPDRV_TASK_PATCH_NONE);
	NUM(patch::DUP, VIPDRV_TASK_PATCH_DUP);
	printf("};\n");
	return 0;
}
