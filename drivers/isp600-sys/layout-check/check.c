/*
 * Generates the layout assertions in drivers/isp600-sys/src/layout.rs from the vendor kernel's
 * camera uAPI header and the ISP600 buffer sizes. Not part of the crate build: run
 * ./run.sh <allwinner-bsp checkout> and it diffs the result against the file.
 *
 * Compiled for the host, which must be 64-bit like the boards: two of the structures hold a
 * pointer.
 */
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdbool.h>

#define __user
typedef uint32_t u32;
typedef uint64_t dma_addr_t;
#define IS_ENABLED(x) 0

#include <linux/sunxi_camera_v2.h>
#include "isp600_reg_cfg.h"

#define SIZE(r, c) printf("    assert!(size_of::<%s>() == %zu);\n", r, sizeof(struct c))
#define OFF(r, c, rf, cf) printf("    assert!(offset_of!(%s, %s) == %zu);\n", r, rf, offsetof(struct c, cf))
#define REQ(n) printf("    assert!(%s == %#lx);\n", #n, (unsigned long)(n))
#define CONST(r, n) printf("    assert!(%s == %#lx);\n", r, (unsigned long)(n))

int main(void)
{
	printf("// sunxi_camera_v2.h and isp600_reg_cfg.h, for a 64-bit target.\n");
	printf("#[cfg(target_pointer_width = \"64\")]\n");
	printf("const _: () = {\n");
	SIZE("TableRegMap", isp_table_reg_map);
	OFF("TableRegMap", isp_table_reg_map, "addr", addr);
	OFF("TableRegMap", isp_table_reg_map, "size", size);
	SIZE("StatData", vin_isp_stat_data);
	OFF("StatData", vin_isp_stat_data, "buf", buf);
	OFF("StatData", vin_isp_stat_data, "buf_size", buf_size);
	OFF("StatData", vin_isp_stat_data, "frame_number", frame_number);
	OFF("StatData", vin_isp_stat_data, "config_counter", config_counter);
	REQ(VIDIOC_VIN_ISP_LOAD_REG);
	REQ(VIDIOC_VIN_ISP_STAT_REQ);
	printf("};\n");
	printf("const _: () = {\n");
	SIZE("H3aConfig", vin_isp_h3a_config);
	OFF("H3aConfig", vin_isp_h3a_config, "buf_size", buf_size);
	OFF("H3aConfig", vin_isp_h3a_config, "config_counter", config_counter);
	SIZE("StatEventStatus", vin_isp_stat_event_status);
	OFF("StatEventStatus", vin_isp_stat_event_status, "frame_number", frame_number);
	OFF("StatEventStatus", vin_isp_stat_event_status, "config_counter", config_counter);
	OFF("StatEventStatus", vin_isp_stat_event_status, "buf_err", buf_err);
	REQ(VIDIOC_VIN_ISP_H3A_CFG);
	REQ(VIDIOC_VIN_ISP_STAT_EN);
	REQ(V4L2_EVENT_VIN_H3A);
	CONST("LOAD_BYTES", ISP_LOAD_DRAM_SIZE);
	CONST("SAVE_BYTES", ISP_STAT_TOTAL_SIZE);
	CONST("SAVE_LOAD_STATISTICS_BYTES", ISP_SAVE_LOAD_STATISTIC_SIZE);
	printf("};\n");
	return 0;
}
