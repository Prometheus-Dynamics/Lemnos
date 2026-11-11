/*
 * Generates the layout assertions in drivers/rknpu/src/uapi.rs from the vendor kernel's NPU uAPI
 * header. Not part of the crate build: run ./run.sh <dir holding rknpu_ioctl.h> and it diffs the
 * result against the file.
 *
 * The header is drivers/rknpu/include/rknpu_ioctl.h from rockchip-linux/kernel develop-6.1 at
 * 77168c8d5ab82399f65a80e9f807b50ba37cf483. It includes <drm/drm.h> for DRM_IOWR, from the
 * system's kernel headers.
 */
#include <stddef.h>
#include <stdio.h>
#include <rknpu_ioctl.h>

/* Rust type, C type. */
#define SIZE(r, c) printf("    assert!(size_of::<%s>() == %zu);\n", #r, sizeof(c))
/* Rust type and field, C type and member. */
#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))
#define NUM(r, c) printf("    assert!(%s == %#llx);\n", #r, (unsigned long long)(c))

int main(void)
{
	printf("const _: () = {\n");
	SIZE(MemCreate, struct rknpu_mem_create);
	OFF(MemCreate, handle, struct rknpu_mem_create, handle);
	OFF(MemCreate, flags, struct rknpu_mem_create, flags);
	OFF(MemCreate, size, struct rknpu_mem_create, size);
	OFF(MemCreate, obj_addr, struct rknpu_mem_create, obj_addr);
	OFF(MemCreate, dma_addr, struct rknpu_mem_create, dma_addr);
	OFF(MemCreate, sram_size, struct rknpu_mem_create, sram_size);
	OFF(MemCreate, iommu_domain_id, struct rknpu_mem_create, iommu_domain_id);
	OFF(MemCreate, core_mask, struct rknpu_mem_create, core_mask);
	SIZE(MemMap, struct rknpu_mem_map);
	OFF(MemMap, handle, struct rknpu_mem_map, handle);
	OFF(MemMap, offset, struct rknpu_mem_map, offset);
	SIZE(MemDestroy, struct rknpu_mem_destroy);
	OFF(MemDestroy, handle, struct rknpu_mem_destroy, handle);
	OFF(MemDestroy, obj_addr, struct rknpu_mem_destroy, obj_addr);
	SIZE(MemSync, struct rknpu_mem_sync);
	OFF(MemSync, flags, struct rknpu_mem_sync, flags);
	OFF(MemSync, obj_addr, struct rknpu_mem_sync, obj_addr);
	OFF(MemSync, offset, struct rknpu_mem_sync, offset);
	OFF(MemSync, size, struct rknpu_mem_sync, size);
	SIZE(Task, struct rknpu_task);
	OFF(Task, flags, struct rknpu_task, flags);
	OFF(Task, op_idx, struct rknpu_task, op_idx);
	OFF(Task, enable_mask, struct rknpu_task, enable_mask);
	OFF(Task, int_mask, struct rknpu_task, int_mask);
	OFF(Task, int_clear, struct rknpu_task, int_clear);
	OFF(Task, int_status, struct rknpu_task, int_status);
	OFF(Task, regcfg_amount, struct rknpu_task, regcfg_amount);
	OFF(Task, regcfg_offset, struct rknpu_task, regcfg_offset);
	OFF(Task, regcmd_addr, struct rknpu_task, regcmd_addr);
	SIZE(SubcoreTask, struct rknpu_subcore_task);
	OFF(SubcoreTask, task_number, struct rknpu_subcore_task, task_number);
	SIZE(Submit, struct rknpu_submit);
	OFF(Submit, flags, struct rknpu_submit, flags);
	OFF(Submit, timeout, struct rknpu_submit, timeout);
	OFF(Submit, task_start, struct rknpu_submit, task_start);
	OFF(Submit, task_number, struct rknpu_submit, task_number);
	OFF(Submit, task_counter, struct rknpu_submit, task_counter);
	OFF(Submit, priority, struct rknpu_submit, priority);
	OFF(Submit, task_obj_addr, struct rknpu_submit, task_obj_addr);
	OFF(Submit, iommu_domain_id, struct rknpu_submit, iommu_domain_id);
	OFF(Submit, reserved, struct rknpu_submit, reserved);
	OFF(Submit, task_base_addr, struct rknpu_submit, task_base_addr);
	OFF(Submit, hw_elapse_time, struct rknpu_submit, hw_elapse_time);
	OFF(Submit, core_mask, struct rknpu_submit, core_mask);
	OFF(Submit, fence_fd, struct rknpu_submit, fence_fd);
	OFF(Submit, subcore_task, struct rknpu_submit, subcore_task);
	SIZE(Action, struct rknpu_action);
	OFF(Action, value, struct rknpu_action, value);
	NUM(IOCTL_ACTION, DRM_IOCTL_RKNPU_ACTION);
	NUM(IOCTL_SUBMIT, DRM_IOCTL_RKNPU_SUBMIT);
	NUM(IOCTL_MEM_CREATE, DRM_IOCTL_RKNPU_MEM_CREATE);
	NUM(IOCTL_MEM_MAP, DRM_IOCTL_RKNPU_MEM_MAP);
	NUM(IOCTL_MEM_DESTROY, DRM_IOCTL_RKNPU_MEM_DESTROY);
	NUM(IOCTL_MEM_SYNC, DRM_IOCTL_RKNPU_MEM_SYNC);
	NUM(mem::NON_CONTIGUOUS, RKNPU_MEM_NON_CONTIGUOUS);
	NUM(mem::CACHEABLE, RKNPU_MEM_CACHEABLE);
	NUM(mem::WRITE_COMBINE, RKNPU_MEM_WRITE_COMBINE);
	NUM(mem::KERNEL_MAPPING, RKNPU_MEM_KERNEL_MAPPING);
	NUM(mem::IOMMU, RKNPU_MEM_IOMMU);
	NUM(mem::ZEROING, RKNPU_MEM_ZEROING);
	NUM(mem::SECURE, RKNPU_MEM_SECURE);
	NUM(mem::DMA32, RKNPU_MEM_DMA32);
	NUM(mem::TRY_ALLOC_SRAM, RKNPU_MEM_TRY_ALLOC_SRAM);
	NUM(mem::TRY_ALLOC_NBUF, RKNPU_MEM_TRY_ALLOC_NBUF);
	NUM(mem::IOMMU_LIMIT_IOVA_ALIGNMENT, RKNPU_MEM_IOMMU_LIMIT_IOVA_ALIGNMENT);
	NUM(sync::TO_DEVICE, RKNPU_MEM_SYNC_TO_DEVICE);
	NUM(sync::FROM_DEVICE, RKNPU_MEM_SYNC_FROM_DEVICE);
	NUM(job::PC, RKNPU_JOB_PC);
	NUM(job::NONBLOCK, RKNPU_JOB_NONBLOCK);
	NUM(job::PINGPONG, RKNPU_JOB_PINGPONG);
	NUM(job::FENCE_IN, RKNPU_JOB_FENCE_IN);
	NUM(job::FENCE_OUT, RKNPU_JOB_FENCE_OUT);
	NUM(action::GET_HW_VERSION, RKNPU_GET_HW_VERSION);
	NUM(action::GET_DRV_VERSION, RKNPU_GET_DRV_VERSION);
	NUM(action::ACT_RESET, RKNPU_ACT_RESET);
	NUM(action::GET_IOMMU_EN, RKNPU_GET_IOMMU_EN);
	NUM(action::SET_PROC_NICE, RKNPU_SET_PROC_NICE);
	NUM(action::GET_IOMMU_DOMAIN_ID, RKNPU_GET_IOMMU_DOMAIN_ID);
	NUM(action::SET_IOMMU_DOMAIN_ID, RKNPU_SET_IOMMU_DOMAIN_ID);
	printf("};\n");
	return 0;
}
