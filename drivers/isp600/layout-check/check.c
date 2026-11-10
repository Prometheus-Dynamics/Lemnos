/*
 * Generates the layout assertions in drivers/isp600/src/layout.rs (the `generated` module) from
 * the vendor kernel's ISP600 headers. Not part of the crate build: run
 * ./run.sh <allwinner-bsp checkout> and it diffs the result against the file.
 *
 * The headers are kernel headers: the few kernel types and IS_ENABLED are stood in for below,
 * with every chip option off, which is the chip the crate describes (neither
 * CONFIG_ARCH_SUN55IW6 nor CONFIG_ARCH_SUN55IW3).
 */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

typedef uint32_t u32;
typedef uint64_t dma_addr_t;
#define IS_ENABLED(x) 0

#include "isp600_reg.h"
#include "isp600_reg_cfg.h"

#define EQ(r, v) printf("    assert!(%s == %#lx);\n", r, (unsigned long)(v))
#define BIT(r, field)                                                                      \
	do {                                                                               \
		ISP_AHB_CFG0_REG_t reg = { 0 };                                            \
		reg.bits.field = 1;                                                        \
		EQ(r, reg.dwval);                                                          \
	} while (0)

int main(void)
{
	printf("// isp600_reg_cfg.h: sizes.\n");
	printf("const _: () = {\n");
	EQ("LOAD_BYTES", ISP_LOAD_DRAM_SIZE);
	EQ("REGISTERS.bytes", ISP_LOAD_REG_SIZE);
	EQ("FRONT_END_TABLE.bytes", ISP_FE_TBL_SIZE);
	EQ("BAYER_TABLE.bytes", ISP_BAYER_TABLE_SIZE);
	EQ("RGB_TABLE.bytes", ISP_RGB_TABLE_SIZE);
	EQ("YUV_TABLE.bytes", ISP_YUV_TABLE_SIZE);
	EQ("LENS_SHADING_TABLE.offset", ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE);
	EQ("LENS_SHADING_TABLE.bytes", ISP_RSC_TBL_SIZE);
	EQ("MSC_TABLE.offset", ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE + ISP_RSC_TBL_SIZE);
	EQ("NR_MSC_TABLE.offset", ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE + 2 * ISP_RSC_TBL_SIZE);
	EQ("NR_MSC_TABLE.bytes", ISP_MSC_NR_TBL_SIZE);
	EQ("D3D_DK_TABLE_BYTES", ISP_D3D_DK_TBL_SIZE);
	EQ("PLTM_TM_TABLE_BYTES", ISP_PLTM_TM_TBL_SIZE);
	EQ("PLTM_LM_TABLE_BYTES", ISP_PLTM_LM_TBL_SIZE);
	EQ("DRC_TABLE_BYTES", ISP_RGB_DRC_TBL_SIZE);
	EQ("GAMMA_TABLE_BYTES", ISP_RGB_GAMMA_TBL_SIZE);
	EQ("CEM_TABLE0_BYTES", ISP_CEM_TBL0_SIZE);
	EQ("CEM_TABLE1_BYTES", ISP_CEM_TBL1_SIZE);
	EQ("SAVE_BYTES", ISP_STAT_TOTAL_SIZE);
	EQ("SAVE_BYTES", ISP_SAVE_REG_SIZE + ISP_STATISTIC_SIZE);
	EQ("STATISTICS_REGIONS[0].bytes", ISP_SAVE_REG_SIZE);
	EQ("STATISTICS_REGIONS[1].bytes", ISP_STAT_AE_MEM_SIZE);
	EQ("STATISTICS_REGIONS[2].bytes", ISP_STAT_RES_SIZE);
	EQ("STATISTICS_REGIONS[3].bytes", ISP_STAT_AF_IIR_ACC_SIZE);
	EQ("STATISTICS_REGIONS[4].bytes", ISP_STAT_AF_FIR_ACC_SIZE);
	EQ("STATISTICS_REGIONS[5].bytes", ISP_STAT_AF_IIR_CNT_SIZE);
	EQ("STATISTICS_REGIONS[6].bytes", ISP_STAT_AF_FIR_CNT_SIZE);
	EQ("STATISTICS_REGIONS[7].bytes", ISP_STAT_AF_HL_CNT_SIZE);
	EQ("STATISTICS_REGIONS[8].bytes", ISP_STAT_AWB_RGB_MEM_SIZE);
	EQ("STATISTICS_REGIONS[9].bytes", ISP_STAT_AFS_MEM_SIZE);
	EQ("STATISTICS_REGIONS[10].bytes", ISP_STAT_HIST0_MEM_SIZE);
	EQ("STATISTICS_REGIONS[11].bytes", ISP_STAT_HIST1_MEM_SIZE);
	EQ("STATISTICS_REGIONS[12].bytes", ISP_STAT_PLTM_LUM_SIZE);
	EQ("STATISTICS_REGIONS[13].bytes", ISP_STAT_PLTM_PKX_SIZE);
	EQ("STATISTICS_REGIONS[14].bytes", ISP_STAT_D3D_K_SIZE);
	EQ("HISTOGRAM_0_OFFSET", ISP_STAT_HIST0_MEM_OFS);
	EQ("SAVE_LOAD_STATISTICS_BYTES", ISP_SAVE_LOAD_STATISTIC_SIZE);
	EQ("AE_WINDOW.offset", ISP_AE_REG_OFS);
	EQ("AE_WINDOW.bytes", ISP_AE_REG_SIZE);
	EQ("AF_WINDOW.offset", ISP_AF_REG_OFS);
	EQ("AF_WINDOW.bytes", ISP_AF_REG_SIZE);
	EQ("AWB_WINDOW.offset", ISP_AWB_REG_OFS);
	EQ("AWB_WINDOW.bytes", ISP_AWB_REG_SIZE);
	EQ("HISTOGRAM_WINDOW.offset", ISP_HIST_REG_OFS);
	EQ("HISTOGRAM_WINDOW.bytes", ISP_HIST_REG_SIZE);
	EQ("VIRTUAL_ISPS", ISP_VIRT_NUM);
	printf("};\n");

	printf("// isp600_reg.h: register offsets.\n");
	printf("const _: () = {\n");
	EQ("AHB_OFFSET", ISP_AHB_REG_OFFSET);
	EQ("AHB_STRIDE", ISP_AHB_AMONG_OFFSET);
	EQ("LOAD_REGISTERS_OFFSET", ISP_LOAD_REG_OFFSET);
	EQ("ahb::CFG0", ISP_AHB_CFG0_REG);
	EQ("ahb::LOAD_ADDR", ISP_LOAD_ADDR_REG);
	EQ("ahb::INT_BYPASS", ISP_INT_BYPASS_REG);
	EQ("ahb::INT_STATUS", ISP_INT_STATUS_REG);
	EQ("ahb::SAVE_ADDR", ISP_SAVE_ADDR_REG);
	EQ("ahb::SAVE_LOAD_ADDR", ISP_SAVE_LOAD_ADDR_REG);
	EQ("reg::GLOBAL_CFG0", ISP_GLOBAL_CFG0_REG);
	EQ("reg::GLOBAL_CFG1", ISP_GLOBAL_CFG1_REG);
	EQ("reg::LBC_TIME_CYCLE", ISP_LBC_TIME_CYCLE_REG);
	EQ("reg::INPUT_SIZE", ISP_INPUT_SIZE_REG);
	EQ("reg::VALID_SIZE", ISP_VALID_SIZE_REG);
	EQ("reg::VALID_START", ISP_VALID_START_REG);
	EQ("reg::MODULE_BYPASS0", ISP_MODULE_BYPASS0_REG);
	EQ("reg::MODULE_BYPASS1", ISP_MODULE_BYPASS1_REG);
	EQ("reg::MODULE_MODE0", ISP_MODULE_MODE0_REG);
	EQ("reg::MODULE_MODE1", ISP_MODULE_MODE1_REG);
	EQ("reg::D3D_BAYER_ADDR", ISP_D3D_BAYER_ADDR_REG);
	EQ("reg::D3D_K0_ADDR", ISP_D3D_K0_ADDR_REG);
	EQ("reg::D3D_K1_ADDR", ISP_D3D_K1_ADDR_REG);
	EQ("reg::D3D_STATUS_ADDR", ISP_D3D_STATUS_ADDR_REG);
	EQ("reg::VIN_CFG0", ISP_VIN_CFG0_REG);
	EQ("reg::CH0_EXPAND_CFG0", ISP_CH0_EXPAND_CFG0_REG);
	EQ("reg::CH1_EXPAND_CFG0", ISP_CH1_EXPAND_CFG0_REG);
	EQ("reg::CH2_EXPAND_CFG0", ISP_CH2_EXPAND_CFG0_REG);
	EQ("reg::D3D_CFG0", ISP_D3D_CFG0_REG);
	EQ("reg::D3D_LBC_CFG0", ISP_D3D_LBC_CFG0_REG);
	EQ("reg::D3D_LBC_CFG5", ISP_D3D_LBC_CFG5_REG);
	printf("};\n");

	printf("// isp600_reg_cfg.h and isp600_reg.h: bits.\n");
	printf("const _: () = {\n");
	EQ("module::WDR_STITCH", WDR_STITCH_EN);
	EQ("module::DPC", DPC_EN);
	EQ("module::CTC", CTC_EN);
	EQ("module::GCA", GCA_EN);
	EQ("module::D2D", D2D_EN);
	EQ("module::D3D", D3D_EN);
	EQ("module::BLC", BLC_EN);
	EQ("module::WB", WB_EN);
	EQ("module::DG", DG_EN);
	EQ("module::RSC", RSC_EN);
	EQ("module::MSC", MSC_EN);
	EQ("module::PLTM", PLTM_EN);
	EQ("module::LCA", LCA_EN);
	EQ("module::SHARP", SHARP_EN);
	EQ("module::CCM", CCM_EN);
	EQ("module::CNR", CNR_EN);
	EQ("module::DRC", DRC_EN);
	EQ("module::GAMMA", GAMMA_EN);
	EQ("module::CEM", CEM_EN);
	EQ("module::AE", AE_EN);
	EQ("module::AF", AF_EN);
	EQ("module::AWB", AWB_EN);
	EQ("module::AFS", AFS_EN);
	EQ("module::HIST0", HIST0_EN);
	EQ("module::HIST1", HIST1_EN);
	EQ("update::LENS", LENS_UPDATE);
	EQ("update::GAMMA", GAMMA_UPDATE);
	EQ("update::DRC", DRC_UPDATE);
	EQ("update::D3D", D3D_UPDATE);
	EQ("update::PLTM", PLTM_UPDATE);
	EQ("update::CEM", CEM_UPDATE);
	EQ("update::MSC", MSC_UPDATE);
	EQ("update::FE0_MSC", FE0_MSC_UPDATE);
	EQ("update::NR_MSC", NR_MSC_UPDATE);
	EQ("update::FE1_MSC", FE1_MSC_UPDATE);
	EQ("update::FE2_MSC", FE2_MSC_UPDATE);
	BIT("update::CAPTURE", cap_en);
	BIT("update::PARA_READY", para_ready);
	BIT("update::LENS", rsc_update);
	BIT("update::GAMMA", gamma_update);
	BIT("update::DRC", drc_update);
	BIT("update::D3D", d3d_update);
	BIT("update::PLTM", pltm_update);
	BIT("update::CEM", cem_update);
	BIT("update::MSC", msc_update);
	BIT("update::FE0_MSC", fe0_msc_update);
	BIT("update::NR_MSC", nr_msc_update);
	BIT("update::FE1_MSC", fe1_msc_update);
	BIT("update::FE2_MSC", fe2_msc_update);
	printf("};\n");
	return 0;
}
