//! The facts each vendor-header variant proves, as the seven `layout-check/check.c` files
//! derived them. Generated once, by reading those files and checking each Rust assertion text
//! against its driver's `layout.rs`; the C files are gone. Do not edit by hand: the record is
//! the list below.

use crate::Variant;

pub static ISP600_DEFAULT: Variant = Variant {
    driver: "isp600",
    name: "default",
    needs: "isp600_reg.h, isp600_reg_cfg.h",
    layout: &["drivers/isp600/src/layout.rs"],
    source: "allwinner-bsp checkout (drivers/vin/vin-isp/isp600 at the pinned rev)",
    defines: &[],
    preamble: r####"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

typedef uint32_t u32;
typedef uint64_t dma_addr_t;
#define IS_ENABLED(x) 0

#include "isp600_reg.h"
#include "isp600_reg_cfg.h"

#define BIT(r, field)                                                                      \
	do {                                                                               \
		ISP_AHB_CFG0_REG_t reg = { 0 };                                            \
		reg.bits.field = 1;                                                        \
		EQ(r, reg.dwval);                                                          \
	} while (0)

"####,
    pairs: &[
        ("LOAD_BYTES", r####"ISP_LOAD_DRAM_SIZE"####),
        ("REGISTERS.bytes", r####"ISP_LOAD_REG_SIZE"####),
        ("FRONT_END_TABLE.bytes", r####"ISP_FE_TBL_SIZE"####),
        ("BAYER_TABLE.bytes", r####"ISP_BAYER_TABLE_SIZE"####),
        ("RGB_TABLE.bytes", r####"ISP_RGB_TABLE_SIZE"####),
        ("YUV_TABLE.bytes", r####"ISP_YUV_TABLE_SIZE"####),
        (
            "LENS_SHADING_TABLE.offset",
            r####"ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE"####,
        ),
        ("LENS_SHADING_TABLE.bytes", r####"ISP_RSC_TBL_SIZE"####),
        (
            "MSC_TABLE.offset",
            r####"ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE + ISP_RSC_TBL_SIZE"####,
        ),
        (
            "NR_MSC_TABLE.offset",
            r####"ISP_LOAD_REG_SIZE + ISP_FE_TBL_SIZE + 2 * ISP_RSC_TBL_SIZE"####,
        ),
        ("NR_MSC_TABLE.bytes", r####"ISP_MSC_NR_TBL_SIZE"####),
        ("D3D_DK_TABLE_BYTES", r####"ISP_D3D_DK_TBL_SIZE"####),
        ("PLTM_TM_TABLE_BYTES", r####"ISP_PLTM_TM_TBL_SIZE"####),
        ("PLTM_LM_TABLE_BYTES", r####"ISP_PLTM_LM_TBL_SIZE"####),
        ("DRC_TABLE_BYTES", r####"ISP_RGB_DRC_TBL_SIZE"####),
        ("GAMMA_TABLE_BYTES", r####"ISP_RGB_GAMMA_TBL_SIZE"####),
        ("CEM_TABLE0_BYTES", r####"ISP_CEM_TBL0_SIZE"####),
        ("CEM_TABLE1_BYTES", r####"ISP_CEM_TBL1_SIZE"####),
        ("SAVE_BYTES", r####"ISP_STAT_TOTAL_SIZE"####),
        (
            "SAVE_BYTES",
            r####"ISP_SAVE_REG_SIZE + ISP_STATISTIC_SIZE"####,
        ),
        ("STATISTICS_REGIONS[0].bytes", r####"ISP_SAVE_REG_SIZE"####),
        (
            "STATISTICS_REGIONS[1].bytes",
            r####"ISP_STAT_AE_MEM_SIZE"####,
        ),
        ("STATISTICS_REGIONS[2].bytes", r####"ISP_STAT_RES_SIZE"####),
        (
            "STATISTICS_REGIONS[3].bytes",
            r####"ISP_STAT_AF_IIR_ACC_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[4].bytes",
            r####"ISP_STAT_AF_FIR_ACC_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[5].bytes",
            r####"ISP_STAT_AF_IIR_CNT_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[6].bytes",
            r####"ISP_STAT_AF_FIR_CNT_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[7].bytes",
            r####"ISP_STAT_AF_HL_CNT_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[8].bytes",
            r####"ISP_STAT_AWB_RGB_MEM_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[9].bytes",
            r####"ISP_STAT_AFS_MEM_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[10].bytes",
            r####"ISP_STAT_HIST0_MEM_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[11].bytes",
            r####"ISP_STAT_HIST1_MEM_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[12].bytes",
            r####"ISP_STAT_PLTM_LUM_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[13].bytes",
            r####"ISP_STAT_PLTM_PKX_SIZE"####,
        ),
        (
            "STATISTICS_REGIONS[14].bytes",
            r####"ISP_STAT_D3D_K_SIZE"####,
        ),
        ("HISTOGRAM_0_OFFSET", r####"ISP_STAT_HIST0_MEM_OFS"####),
        (
            "SAVE_LOAD_STATISTICS_BYTES",
            r####"ISP_SAVE_LOAD_STATISTIC_SIZE"####,
        ),
        ("AE_WINDOW.offset", r####"ISP_AE_REG_OFS"####),
        ("AE_WINDOW.bytes", r####"ISP_AE_REG_SIZE"####),
        ("AF_WINDOW.offset", r####"ISP_AF_REG_OFS"####),
        ("AF_WINDOW.bytes", r####"ISP_AF_REG_SIZE"####),
        ("AWB_WINDOW.offset", r####"ISP_AWB_REG_OFS"####),
        ("AWB_WINDOW.bytes", r####"ISP_AWB_REG_SIZE"####),
        ("HISTOGRAM_WINDOW.offset", r####"ISP_HIST_REG_OFS"####),
        ("HISTOGRAM_WINDOW.bytes", r####"ISP_HIST_REG_SIZE"####),
        ("VIRTUAL_ISPS", r####"ISP_VIRT_NUM"####),
        ("AHB_OFFSET", r####"ISP_AHB_REG_OFFSET"####),
        ("AHB_STRIDE", r####"ISP_AHB_AMONG_OFFSET"####),
        ("LOAD_REGISTERS_OFFSET", r####"ISP_LOAD_REG_OFFSET"####),
        ("ahb::CFG0", r####"ISP_AHB_CFG0_REG"####),
        ("ahb::LOAD_ADDR", r####"ISP_LOAD_ADDR_REG"####),
        ("ahb::INT_BYPASS", r####"ISP_INT_BYPASS_REG"####),
        ("ahb::INT_STATUS", r####"ISP_INT_STATUS_REG"####),
        ("ahb::SAVE_ADDR", r####"ISP_SAVE_ADDR_REG"####),
        ("ahb::SAVE_LOAD_ADDR", r####"ISP_SAVE_LOAD_ADDR_REG"####),
        ("reg::GLOBAL_CFG0", r####"ISP_GLOBAL_CFG0_REG"####),
        ("reg::GLOBAL_CFG1", r####"ISP_GLOBAL_CFG1_REG"####),
        ("reg::LBC_TIME_CYCLE", r####"ISP_LBC_TIME_CYCLE_REG"####),
        ("reg::INPUT_SIZE", r####"ISP_INPUT_SIZE_REG"####),
        ("reg::VALID_SIZE", r####"ISP_VALID_SIZE_REG"####),
        ("reg::VALID_START", r####"ISP_VALID_START_REG"####),
        ("reg::MODULE_BYPASS0", r####"ISP_MODULE_BYPASS0_REG"####),
        ("reg::MODULE_BYPASS1", r####"ISP_MODULE_BYPASS1_REG"####),
        ("reg::MODULE_MODE0", r####"ISP_MODULE_MODE0_REG"####),
        ("reg::MODULE_MODE1", r####"ISP_MODULE_MODE1_REG"####),
        ("reg::D3D_BAYER_ADDR", r####"ISP_D3D_BAYER_ADDR_REG"####),
        ("reg::D3D_K0_ADDR", r####"ISP_D3D_K0_ADDR_REG"####),
        ("reg::D3D_K1_ADDR", r####"ISP_D3D_K1_ADDR_REG"####),
        ("reg::D3D_STATUS_ADDR", r####"ISP_D3D_STATUS_ADDR_REG"####),
        ("reg::VIN_CFG0", r####"ISP_VIN_CFG0_REG"####),
        ("reg::CH0_EXPAND_CFG0", r####"ISP_CH0_EXPAND_CFG0_REG"####),
        ("reg::CH1_EXPAND_CFG0", r####"ISP_CH1_EXPAND_CFG0_REG"####),
        ("reg::CH2_EXPAND_CFG0", r####"ISP_CH2_EXPAND_CFG0_REG"####),
        ("reg::D3D_CFG0", r####"ISP_D3D_CFG0_REG"####),
        ("reg::D3D_LBC_CFG0", r####"ISP_D3D_LBC_CFG0_REG"####),
        ("reg::D3D_LBC_CFG5", r####"ISP_D3D_LBC_CFG5_REG"####),
        ("module::WDR_STITCH", r####"WDR_STITCH_EN"####),
        ("module::DPC", r####"DPC_EN"####),
        ("module::CTC", r####"CTC_EN"####),
        ("module::GCA", r####"GCA_EN"####),
        ("module::D2D", r####"D2D_EN"####),
        ("module::D3D", r####"D3D_EN"####),
        ("module::BLC", r####"BLC_EN"####),
        ("module::WB", r####"WB_EN"####),
        ("module::DG", r####"DG_EN"####),
        ("module::RSC", r####"RSC_EN"####),
        ("module::MSC", r####"MSC_EN"####),
        ("module::PLTM", r####"PLTM_EN"####),
        ("module::LCA", r####"LCA_EN"####),
        ("module::SHARP", r####"SHARP_EN"####),
        ("module::CCM", r####"CCM_EN"####),
        ("module::CNR", r####"CNR_EN"####),
        ("module::DRC", r####"DRC_EN"####),
        ("module::GAMMA", r####"GAMMA_EN"####),
        ("module::CEM", r####"CEM_EN"####),
        ("module::AE", r####"AE_EN"####),
        ("module::AF", r####"AF_EN"####),
        ("module::AWB", r####"AWB_EN"####),
        ("module::AFS", r####"AFS_EN"####),
        ("module::HIST0", r####"HIST0_EN"####),
        ("module::HIST1", r####"HIST1_EN"####),
        ("update::LENS", r####"LENS_UPDATE"####),
        ("update::GAMMA", r####"GAMMA_UPDATE"####),
        ("update::DRC", r####"DRC_UPDATE"####),
        ("update::D3D", r####"D3D_UPDATE"####),
        ("update::PLTM", r####"PLTM_UPDATE"####),
        ("update::CEM", r####"CEM_UPDATE"####),
        ("update::MSC", r####"MSC_UPDATE"####),
        ("update::FE0_MSC", r####"FE0_MSC_UPDATE"####),
        ("update::NR_MSC", r####"NR_MSC_UPDATE"####),
        ("update::FE1_MSC", r####"FE1_MSC_UPDATE"####),
        ("update::FE2_MSC", r####"FE2_MSC_UPDATE"####),
    ],
    record: "unknown",
    unmapped: &[
        "BIT(\"update::CAPTURE\", cap_en);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::PARA_READY\", para_ready);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::LENS\", rsc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::GAMMA\", gamma_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::DRC\", drc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::D3D\", d3d_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::PLTM\", pltm_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::CEM\", cem_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::MSC\", msc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::FE0_MSC\", fe0_msc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::NR_MSC\", nr_msc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::FE1_MSC\", fe1_msc_update);: not a constant expression (set through a struct field at run time)",
        "BIT(\"update::FE2_MSC\", fe2_msc_update);: not a constant expression (set through a struct field at run time)",
    ],
};

pub static ISP600_SYS_DEFAULT: Variant = Variant {
    driver: "isp600-sys",
    name: "default",
    needs: "linux/sunxi_camera_v2.h and isp600_reg_cfg.h",
    layout: &["drivers/isp600-sys/src/layout.rs"],
    source: "allwinner-bsp checkout (include/uapi/media/sunxi_camera_v2.h)",
    defines: &[],
    preamble: r####"
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


"####,
    pairs: &[
        (
            "size_of::<TableRegMap>()",
            r####"sizeof(struct isp_table_reg_map)"####,
        ),
        (
            "offset_of!(TableRegMap, addr)",
            r####"offsetof(struct isp_table_reg_map, addr)"####,
        ),
        (
            "offset_of!(TableRegMap, size)",
            r####"offsetof(struct isp_table_reg_map, size)"####,
        ),
        (
            "size_of::<StatData>()",
            r####"sizeof(struct vin_isp_stat_data)"####,
        ),
        (
            "offset_of!(StatData, buf)",
            r####"offsetof(struct vin_isp_stat_data, buf)"####,
        ),
        (
            "offset_of!(StatData, buf_size)",
            r####"offsetof(struct vin_isp_stat_data, buf_size)"####,
        ),
        (
            "offset_of!(StatData, frame_number)",
            r####"offsetof(struct vin_isp_stat_data, frame_number)"####,
        ),
        (
            "offset_of!(StatData, config_counter)",
            r####"offsetof(struct vin_isp_stat_data, config_counter)"####,
        ),
        (
            "VIDIOC_VIN_ISP_LOAD_REG",
            r####"VIDIOC_VIN_ISP_LOAD_REG"####,
        ),
        (
            "VIDIOC_VIN_ISP_STAT_REQ",
            r####"VIDIOC_VIN_ISP_STAT_REQ"####,
        ),
        (
            "size_of::<H3aConfig>()",
            r####"sizeof(struct vin_isp_h3a_config)"####,
        ),
        (
            "offset_of!(H3aConfig, buf_size)",
            r####"offsetof(struct vin_isp_h3a_config, buf_size)"####,
        ),
        (
            "offset_of!(H3aConfig, config_counter)",
            r####"offsetof(struct vin_isp_h3a_config, config_counter)"####,
        ),
        (
            "size_of::<StatEventStatus>()",
            r####"sizeof(struct vin_isp_stat_event_status)"####,
        ),
        (
            "offset_of!(StatEventStatus, frame_number)",
            r####"offsetof(struct vin_isp_stat_event_status, frame_number)"####,
        ),
        (
            "offset_of!(StatEventStatus, config_counter)",
            r####"offsetof(struct vin_isp_stat_event_status, config_counter)"####,
        ),
        (
            "offset_of!(StatEventStatus, buf_err)",
            r####"offsetof(struct vin_isp_stat_event_status, buf_err)"####,
        ),
        ("VIDIOC_VIN_ISP_H3A_CFG", r####"VIDIOC_VIN_ISP_H3A_CFG"####),
        ("VIDIOC_VIN_ISP_STAT_EN", r####"VIDIOC_VIN_ISP_STAT_EN"####),
        ("V4L2_EVENT_VIN_H3A", r####"V4L2_EVENT_VIN_H3A"####),
        ("LOAD_BYTES", r####"ISP_LOAD_DRAM_SIZE"####),
        ("SAVE_BYTES", r####"ISP_STAT_TOTAL_SIZE"####),
        (
            "SAVE_LOAD_STATISTICS_BYTES",
            r####"ISP_SAVE_LOAD_STATISTIC_SIZE"####,
        ),
    ],
    record: "unknown",
    unmapped: &[],
};

pub static RKAIISP_DEFAULT: Variant = Variant {
    driver: "rkaiisp",
    name: "default",
    needs: "linux/rk-aiisp-config.h and linux/rk-isp2-config.h",
    layout: &["drivers/rkaiisp/src/layout.rs"],
    source: "rockchip-linux/kernel develop-6.1 include/uapi/linux",
    defines: &[],
    preamble: r####"
#include <stddef.h>
#include <stdio.h>
#include <linux/videodev2.h>
#include <linux/rk-aiisp-config.h>



#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))

"####,
    pairs: &[
        (
            "size_of::<ParamInfo>()",
            r####"sizeof(struct rkaiisp_param_info)"####,
        ),
        (
            "offset_of!(ParamInfo, exemode)",
            r####"offsetof(struct rkaiisp_param_info, exemode)"####,
        ),
        (
            "offset_of!(ParamInfo, para_size)",
            r####"offsetof(struct rkaiisp_param_info, para_size)"####,
        ),
        (
            "offset_of!(ParamInfo, max_runcnt)",
            r####"offsetof(struct rkaiisp_param_info, max_runcnt)"####,
        ),
        (
            "size_of::<AirmsQueue>()",
            r####"sizeof(struct rkaiisp_airms_st)"####,
        ),
        (
            "offset_of!(AirmsQueue, inbuf_idx)",
            r####"offsetof(struct rkaiisp_airms_st, inbuf_idx)"####,
        ),
        (
            "offset_of!(AirmsQueue, outbuf_idx)",
            r####"offsetof(struct rkaiisp_airms_st, outbuf_idx)"####,
        ),
        (
            "size_of::<AibnrQueue>()",
            r####"sizeof(struct rkisp_aiisp_st)"####,
        ),
        (
            "offset_of!(AibnrQueue, sequence)",
            r####"offsetof(struct rkisp_aiisp_st, sequence)"####,
        ),
        (
            "offset_of!(AibnrQueue, iir_index)",
            r####"offsetof(struct rkisp_aiisp_st, iir_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, gain_index)",
            r####"offsetof(struct rkisp_aiisp_st, gain_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, aiisp_index)",
            r####"offsetof(struct rkisp_aiisp_st, aiisp_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, aipre_gain_index)",
            r####"offsetof(struct rkisp_aiisp_st, aipre_gain_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, vpsl_index)",
            r####"offsetof(struct rkisp_aiisp_st, vpsl_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, y_src_index)",
            r####"offsetof(struct rkisp_aiisp_st, y_src_index)"####,
        ),
        (
            "offset_of!(AibnrQueue, y_dest_index)",
            r####"offsetof(struct rkisp_aiisp_st, y_dest_index)"####,
        ),
        (
            "QUEUE_BUF_BYTES",
            r####"sizeof(union rkaiisp_queue_buf)"####,
        ),
        (
            "offset_of!(AibnrQueue, sequence)",
            r####"offsetof(union rkaiisp_queue_buf, aibnr_st.sequence)"####,
        ),
        (
            "offset_of!(AirmsQueue, sequence)",
            r####"offsetof(union rkaiisp_queue_buf, airms_st.sequence)"####,
        ),
        (
            "size_of::<IspBuf>()",
            r####"sizeof(struct rkisp_buf_info)"####,
        ),
        (
            "offset_of!(IspBuf, buf_size)",
            r####"offsetof(struct rkisp_buf_info, buf_size)"####,
        ),
        (
            "offset_of!(IspBuf, buf_stride)",
            r####"offsetof(struct rkisp_buf_info, buf_stride)"####,
        ),
        (
            "offset_of!(IspBuf, buf_fd)",
            r####"offsetof(struct rkisp_buf_info, buf_fd)"####,
        ),
        (
            "size_of::<BnrBufInfo>()",
            r####"sizeof(struct rkisp_bnr_buf_info)"####,
        ),
        (
            "offset_of!(BnrBufInfo, ds)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.ds)"####,
        ),
        (
            "offset_of!(BnrBufInfo, wgt)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.wgt)"####,
        ),
        (
            "offset_of!(BnrBufInfo, aiisp)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.aiisp)"####,
        ),
        (
            "offset_of!(BnrBufInfo, gain)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.gain)"####,
        ),
        (
            "offset_of!(BnrBufInfo, aipre_gain)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.aipre_gain)"####,
        ),
        (
            "offset_of!(BnrBufInfo, vpsl)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.vpsl)"####,
        ),
        (
            "offset_of!(BnrBufInfo, y_src)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.y_src)"####,
        ),
        (
            "offset_of!(BnrBufInfo, iir_rw_fmt)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.iir_rw_fmt)"####,
        ),
        (
            "offset_of!(BnrBufInfo, aibnr_l2)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.aibnr_l2)"####,
        ),
        (
            "offset_of!(BnrBufInfo, vpsl_yraw_offs)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.vpsl_yraw_offs)"####,
        ),
        (
            "offset_of!(BnrBufInfo, vpsl_yraw_stride)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.vpsl_yraw_stride)"####,
        ),
        (
            "offset_of!(BnrBufInfo, vpsl_sig_offs)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.vpsl_sig_offs)"####,
        ),
        (
            "offset_of!(BnrBufInfo, vpsl_sig_stride)",
            r####"offsetof(struct rkisp_bnr_buf_info, u.v35.vpsl_sig_stride)"####,
        ),
        (
            "size_of::<IspbufInfo>()",
            r####"sizeof(struct rkaiisp_ispbuf_info)"####,
        ),
        (
            "offset_of!(IspbufInfo, iir_width)",
            r####"offsetof(struct rkaiisp_ispbuf_info, iir_width)"####,
        ),
        (
            "offset_of!(IspbufInfo, raw_width)",
            r####"offsetof(struct rkaiisp_ispbuf_info, raw_width)"####,
        ),
        (
            "offset_of!(IspbufInfo, raw_height)",
            r####"offsetof(struct rkaiisp_ispbuf_info, raw_height)"####,
        ),
        (
            "offset_of!(IspbufInfo, sig_width)",
            r####"offsetof(struct rkaiisp_ispbuf_info, sig_width)"####,
        ),
        (
            "offset_of!(IspbufInfo, sig_height)",
            r####"offsetof(struct rkaiisp_ispbuf_info, sig_height)"####,
        ),
        (
            "offset_of!(IspbufInfo, narmap_width)",
            r####"offsetof(struct rkaiisp_ispbuf_info, narmap_width)"####,
        ),
        (
            "offset_of!(IspbufInfo, narmap_height)",
            r####"offsetof(struct rkaiisp_ispbuf_info, narmap_height)"####,
        ),
        (
            "size_of::<RmsbufInfo>()",
            r####"sizeof(struct rkaiisp_rmsbuf_info)"####,
        ),
        (
            "offset_of!(RmsbufInfo, inbuf_num)",
            r####"offsetof(struct rkaiisp_rmsbuf_info, inbuf_num)"####,
        ),
        (
            "offset_of!(RmsbufInfo, inbuf_fd)",
            r####"offsetof(struct rkaiisp_rmsbuf_info, inbuf_fd)"####,
        ),
        (
            "offset_of!(RmsbufInfo, outbuf_fd)",
            r####"offsetof(struct rkaiisp_rmsbuf_info, outbuf_fd)"####,
        ),
        (
            "size_of::<YnrbufInfo>()",
            r####"sizeof(struct rkaiisp_ynrbuf_info)"####,
        ),
        (
            "offset_of!(YnrbufInfo, buf_cnt)",
            r####"offsetof(struct rkaiisp_ynrbuf_info, buf_cnt)"####,
        ),
        (
            "offset_of!(YnrbufInfo, dma_fd)",
            r####"offsetof(struct rkaiisp_ynrbuf_info, dma_fd)"####,
        ),
        (
            "size_of::<OtherCfg>()",
            r####"sizeof(struct rkaiisp_other_cfg)"####,
        ),
        (
            "offset_of!(OtherCfg, sw_post_blacklvl)",
            r####"offsetof(struct rkaiisp_other_cfg, sw_post_blacklvl)"####,
        ),
        (
            "offset_of!(OtherCfg, sw_in_comp_y)",
            r####"offsetof(struct rkaiisp_other_cfg, sw_in_comp_y)"####,
        ),
        (
            "offset_of!(OtherCfg, sw_out_decomp_y)",
            r####"offsetof(struct rkaiisp_other_cfg, sw_out_decomp_y)"####,
        ),
        (
            "size_of::<ModelCfg>()",
            r####"sizeof(struct rkaiisp_model_cfg)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_level_num)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_level_num)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_l1_level_num)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_l1_level_num)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_op_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_op_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_drop_en)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_drop_en)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_lv_active)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_lv_active)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_aiisp_lv_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_aiisp_lv_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn_en)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn_en)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn_num)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn_num)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn_data_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn_data_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn1_sel)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn1_sel)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_mi_chn3_sel)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_mi_chn3_sel)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_out_d2s_en)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_out_d2s_en)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_out_mode)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_out_mode)"####,
        ),
        (
            "offset_of!(ModelCfg, sw_lastlvlm1_clip8bit)",
            r####"offsetof(struct rkaiisp_model_cfg, sw_lastlvlm1_clip8bit)"####,
        ),
        (
            "size_of::<KwtCfg>()",
            r####"sizeof(struct rkaiisp_kwt_cfg)"####,
        ),
        (
            "offset_of!(KwtCfg, kwt_size)",
            r####"offsetof(struct rkaiisp_kwt_cfg, kwt_size)"####,
        ),
        (
            "offset_of!(KwtCfg, kwt_pad_size)",
            r####"offsetof(struct rkaiisp_kwt_cfg, kwt_pad_size)"####,
        ),
        (
            "size_of::<Params>()",
            r####"sizeof(struct rkaiisp_params)"####,
        ),
        (
            "offset_of!(Params, module_update)",
            r####"offsetof(struct rkaiisp_params, module_update)"####,
        ),
        (
            "offset_of!(Params, model_runcnt)",
            r####"offsetof(struct rkaiisp_params, model_runcnt)"####,
        ),
        (
            "offset_of!(Params, model_mode)",
            r####"offsetof(struct rkaiisp_params, model_mode)"####,
        ),
        (
            "offset_of!(Params, other_cfg)",
            r####"offsetof(struct rkaiisp_params, other_cfg)"####,
        ),
        (
            "offset_of!(Params, model_cfg)",
            r####"offsetof(struct rkaiisp_params, model_cfg)"####,
        ),
        (
            "offset_of!(Params, kwt_cfg)",
            r####"offsetof(struct rkaiisp_params, kwt_cfg)"####,
        ),
        (
            "offset_of!(Params, reserved)",
            r####"offsetof(struct rkaiisp_params, reserved)"####,
        ),
        (
            "size_of::<ModelInfo>()",
            r####"sizeof(struct rkaiisp_model_info)"####,
        ),
        (
            "offset_of!(ModelInfo, model_qr)",
            r####"offsetof(struct rkaiisp_model_info, model_qr)"####,
        ),
        (
            "offset_of!(ModelInfo, model_mode)",
            r####"offsetof(struct rkaiisp_model_info, model_mode)"####,
        ),
        (
            "offset_of!(ModelInfo, model_cfg)",
            r####"offsetof(struct rkaiisp_model_info, model_cfg)"####,
        ),
        (
            "offset_of!(ModelInfo, kwt_cfg)",
            r####"offsetof(struct rkaiisp_model_info, kwt_cfg)"####,
        ),
        (
            "offset_of!(ModelInfo, reserved)",
            r####"offsetof(struct rkaiisp_model_info, reserved)"####,
        ),
        ("CMD_SET_PARAM_INFO", r####"RKAIISP_CMD_SET_PARAM_INFO"####),
        ("CMD_INIT_BUFPOOL", r####"RKAIISP_CMD_INIT_BUFPOOL"####),
        ("CMD_FREE_BUFPOOL", r####"RKAIISP_CMD_FREE_BUFPOOL"####),
        ("CMD_QUEUE_BUF", r####"RKAIISP_CMD_QUEUE_BUF"####),
        (
            "CMD_INIT_AIRMS_BUFPOOL",
            r####"RKAIISP_CMD_INIT_AIRMS_BUFPOOL"####,
        ),
        (
            "CMD_GET_YNRBUF_INFO",
            r####"RKAIISP_CMD_GET_YNRBUF_INFO"####,
        ),
        (
            "CMD_SET_MEMORY_MODE",
            r####"RKAIISP_CMD_SET_MEMORY_MODE"####,
        ),
        ("CMD_CLEAR_IQPARAMS", r####"RKAIISP_CMD_CLEAR_IQPARAMS"####),
        ("EVENT_AIISP_DONE", r####"RKAIISP_V4L2_EVENT_AIISP_DONE"####),
        ("META_FORMAT", r####"V4L2_META_FMT_RK_ISP1_PARAMS"####),
        ("MAX_RUNCNT", r####"RKAIISP_MAX_RUNCNT"####),
        ("MAX_ISPBUF", r####"RKAIISP_MAX_ISPBUF"####),
        ("MODEL_UPDATE", r####"RKAIISP_MODEL_UPDATE"####),
        ("OTHER_UPDATE", r####"RKAIISP_OTHER_UPDATE"####),
        ("AIYNR_YBUF_NUM_MAX", r####"RKAIISP_AIYNR_YBUF_NUM_MAX"####),
        ("ISP_BUFFER_MAX", r####"RKISP_BUFFER_MAX"####),
        ("VPSL_YRAW_CHN_MAX", r####"VPSL_YRAW_CHN_MAX"####),
        ("VPSL_SIG_CHN_MAX", r####"VPSL_SIG_CHN_MAX"####),
        ("chn_src::ISP_IIR", r####"ISP_IIR"####),
        ("chn_src::ISP_AIPRE_NARMAP", r####"ISP_AIPRE_NARMAP"####),
        ("chn_src::AIISP_LAST_OUT", r####"AIISP_LAST_OUT"####),
        (
            "chn_src::VICAP_BAYER_RAW_DOWN",
            r####"VICAP_BAYER_RAW_DOWN"####,
        ),
        ("exealgo::AIYNR", r####"AIYNR"####),
        ("model_mode::COMBO_MODE", r####"COMBO_MODE"####),
        ("model_mode::AIYNR_MODE", r####"AIYNR_MODE"####),
        (
            "exemode::BOTHEVENT_IN_KERNEL",
            r####"BOTHEVENT_IN_KERNEL"####,
        ),
        ("mem_mode::COMBO_MEMODE", r####"COMBO_MEMODE"####),
    ],
    record: "develop-6.1",
    unmapped: &[],
};

pub static RKNPU_SYS_DEFAULT: Variant = Variant {
    driver: "rknpu-sys",
    name: "default",
    needs: "rknpu_ioctl.h (and drm/drm.h from the kernel headers)",
    layout: &["drivers/rknpu-sys/src/uapi.rs"],
    source: "rockchip-linux/kernel develop-6.1 drivers/npu/include/rknpu_ioctl.h at 77168c8d5ab82399f65a80e9f807b50ba37cf483",
    defines: &[],
    preamble: r####"
#include <stddef.h>
#include <stdio.h>
#include <rknpu_ioctl.h>



#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))

"####,
    pairs: &[
        (
            "size_of::<MemCreate>()",
            r####"sizeof(struct rknpu_mem_create)"####,
        ),
        (
            "offset_of!(MemCreate, handle)",
            r####"offsetof(struct rknpu_mem_create, handle)"####,
        ),
        (
            "offset_of!(MemCreate, flags)",
            r####"offsetof(struct rknpu_mem_create, flags)"####,
        ),
        (
            "offset_of!(MemCreate, size)",
            r####"offsetof(struct rknpu_mem_create, size)"####,
        ),
        (
            "offset_of!(MemCreate, obj_addr)",
            r####"offsetof(struct rknpu_mem_create, obj_addr)"####,
        ),
        (
            "offset_of!(MemCreate, dma_addr)",
            r####"offsetof(struct rknpu_mem_create, dma_addr)"####,
        ),
        (
            "offset_of!(MemCreate, sram_size)",
            r####"offsetof(struct rknpu_mem_create, sram_size)"####,
        ),
        (
            "offset_of!(MemCreate, iommu_domain_id)",
            r####"offsetof(struct rknpu_mem_create, iommu_domain_id)"####,
        ),
        (
            "offset_of!(MemCreate, core_mask)",
            r####"offsetof(struct rknpu_mem_create, core_mask)"####,
        ),
        (
            "size_of::<MemMap>()",
            r####"sizeof(struct rknpu_mem_map)"####,
        ),
        (
            "offset_of!(MemMap, handle)",
            r####"offsetof(struct rknpu_mem_map, handle)"####,
        ),
        (
            "offset_of!(MemMap, offset)",
            r####"offsetof(struct rknpu_mem_map, offset)"####,
        ),
        (
            "size_of::<MemDestroy>()",
            r####"sizeof(struct rknpu_mem_destroy)"####,
        ),
        (
            "offset_of!(MemDestroy, handle)",
            r####"offsetof(struct rknpu_mem_destroy, handle)"####,
        ),
        (
            "offset_of!(MemDestroy, obj_addr)",
            r####"offsetof(struct rknpu_mem_destroy, obj_addr)"####,
        ),
        (
            "size_of::<MemSync>()",
            r####"sizeof(struct rknpu_mem_sync)"####,
        ),
        (
            "offset_of!(MemSync, flags)",
            r####"offsetof(struct rknpu_mem_sync, flags)"####,
        ),
        (
            "offset_of!(MemSync, obj_addr)",
            r####"offsetof(struct rknpu_mem_sync, obj_addr)"####,
        ),
        (
            "offset_of!(MemSync, offset)",
            r####"offsetof(struct rknpu_mem_sync, offset)"####,
        ),
        (
            "offset_of!(MemSync, size)",
            r####"offsetof(struct rknpu_mem_sync, size)"####,
        ),
        ("size_of::<Task>()", r####"sizeof(struct rknpu_task)"####),
        (
            "offset_of!(Task, flags)",
            r####"offsetof(struct rknpu_task, flags)"####,
        ),
        (
            "offset_of!(Task, op_idx)",
            r####"offsetof(struct rknpu_task, op_idx)"####,
        ),
        (
            "offset_of!(Task, enable_mask)",
            r####"offsetof(struct rknpu_task, enable_mask)"####,
        ),
        (
            "offset_of!(Task, int_mask)",
            r####"offsetof(struct rknpu_task, int_mask)"####,
        ),
        (
            "offset_of!(Task, int_clear)",
            r####"offsetof(struct rknpu_task, int_clear)"####,
        ),
        (
            "offset_of!(Task, int_status)",
            r####"offsetof(struct rknpu_task, int_status)"####,
        ),
        (
            "offset_of!(Task, regcfg_amount)",
            r####"offsetof(struct rknpu_task, regcfg_amount)"####,
        ),
        (
            "offset_of!(Task, regcfg_offset)",
            r####"offsetof(struct rknpu_task, regcfg_offset)"####,
        ),
        (
            "offset_of!(Task, regcmd_addr)",
            r####"offsetof(struct rknpu_task, regcmd_addr)"####,
        ),
        (
            "size_of::<SubcoreTask>()",
            r####"sizeof(struct rknpu_subcore_task)"####,
        ),
        (
            "offset_of!(SubcoreTask, task_number)",
            r####"offsetof(struct rknpu_subcore_task, task_number)"####,
        ),
        (
            "size_of::<Submit>()",
            r####"sizeof(struct rknpu_submit)"####,
        ),
        (
            "offset_of!(Submit, flags)",
            r####"offsetof(struct rknpu_submit, flags)"####,
        ),
        (
            "offset_of!(Submit, timeout)",
            r####"offsetof(struct rknpu_submit, timeout)"####,
        ),
        (
            "offset_of!(Submit, task_start)",
            r####"offsetof(struct rknpu_submit, task_start)"####,
        ),
        (
            "offset_of!(Submit, task_number)",
            r####"offsetof(struct rknpu_submit, task_number)"####,
        ),
        (
            "offset_of!(Submit, task_counter)",
            r####"offsetof(struct rknpu_submit, task_counter)"####,
        ),
        (
            "offset_of!(Submit, priority)",
            r####"offsetof(struct rknpu_submit, priority)"####,
        ),
        (
            "offset_of!(Submit, task_obj_addr)",
            r####"offsetof(struct rknpu_submit, task_obj_addr)"####,
        ),
        (
            "offset_of!(Submit, iommu_domain_id)",
            r####"offsetof(struct rknpu_submit, iommu_domain_id)"####,
        ),
        (
            "offset_of!(Submit, reserved)",
            r####"offsetof(struct rknpu_submit, reserved)"####,
        ),
        (
            "offset_of!(Submit, task_base_addr)",
            r####"offsetof(struct rknpu_submit, task_base_addr)"####,
        ),
        (
            "offset_of!(Submit, hw_elapse_time)",
            r####"offsetof(struct rknpu_submit, hw_elapse_time)"####,
        ),
        (
            "offset_of!(Submit, core_mask)",
            r####"offsetof(struct rknpu_submit, core_mask)"####,
        ),
        (
            "offset_of!(Submit, fence_fd)",
            r####"offsetof(struct rknpu_submit, fence_fd)"####,
        ),
        (
            "offset_of!(Submit, subcore_task)",
            r####"offsetof(struct rknpu_submit, subcore_task)"####,
        ),
        (
            "size_of::<Action>()",
            r####"sizeof(struct rknpu_action)"####,
        ),
        (
            "offset_of!(Action, value)",
            r####"offsetof(struct rknpu_action, value)"####,
        ),
        ("IOCTL_ACTION", r####"DRM_IOCTL_RKNPU_ACTION"####),
        ("IOCTL_SUBMIT", r####"DRM_IOCTL_RKNPU_SUBMIT"####),
        ("IOCTL_MEM_CREATE", r####"DRM_IOCTL_RKNPU_MEM_CREATE"####),
        ("IOCTL_MEM_MAP", r####"DRM_IOCTL_RKNPU_MEM_MAP"####),
        ("IOCTL_MEM_DESTROY", r####"DRM_IOCTL_RKNPU_MEM_DESTROY"####),
        ("IOCTL_MEM_SYNC", r####"DRM_IOCTL_RKNPU_MEM_SYNC"####),
        ("mem::NON_CONTIGUOUS", r####"RKNPU_MEM_NON_CONTIGUOUS"####),
        ("mem::CACHEABLE", r####"RKNPU_MEM_CACHEABLE"####),
        ("mem::WRITE_COMBINE", r####"RKNPU_MEM_WRITE_COMBINE"####),
        ("mem::KERNEL_MAPPING", r####"RKNPU_MEM_KERNEL_MAPPING"####),
        ("mem::IOMMU", r####"RKNPU_MEM_IOMMU"####),
        ("mem::ZEROING", r####"RKNPU_MEM_ZEROING"####),
        ("mem::SECURE", r####"RKNPU_MEM_SECURE"####),
        ("mem::DMA32", r####"RKNPU_MEM_DMA32"####),
        ("mem::TRY_ALLOC_SRAM", r####"RKNPU_MEM_TRY_ALLOC_SRAM"####),
        ("mem::TRY_ALLOC_NBUF", r####"RKNPU_MEM_TRY_ALLOC_NBUF"####),
        (
            "mem::IOMMU_LIMIT_IOVA_ALIGNMENT",
            r####"RKNPU_MEM_IOMMU_LIMIT_IOVA_ALIGNMENT"####,
        ),
        ("sync::TO_DEVICE", r####"RKNPU_MEM_SYNC_TO_DEVICE"####),
        ("sync::FROM_DEVICE", r####"RKNPU_MEM_SYNC_FROM_DEVICE"####),
        ("job::PC", r####"RKNPU_JOB_PC"####),
        ("job::NONBLOCK", r####"RKNPU_JOB_NONBLOCK"####),
        ("job::PINGPONG", r####"RKNPU_JOB_PINGPONG"####),
        ("job::FENCE_IN", r####"RKNPU_JOB_FENCE_IN"####),
        ("job::FENCE_OUT", r####"RKNPU_JOB_FENCE_OUT"####),
        ("action::GET_HW_VERSION", r####"RKNPU_GET_HW_VERSION"####),
        ("action::GET_DRV_VERSION", r####"RKNPU_GET_DRV_VERSION"####),
        ("action::ACT_RESET", r####"RKNPU_ACT_RESET"####),
        ("action::GET_IOMMU_EN", r####"RKNPU_GET_IOMMU_EN"####),
        ("action::SET_PROC_NICE", r####"RKNPU_SET_PROC_NICE"####),
        (
            "action::GET_IOMMU_DOMAIN_ID",
            r####"RKNPU_GET_IOMMU_DOMAIN_ID"####,
        ),
        (
            "action::SET_IOMMU_DOMAIN_ID",
            r####"RKNPU_SET_IOMMU_DOMAIN_ID"####,
        ),
    ],
    record: "77168c8d5ab82399f65a80e9f807b50ba37cf483",
    unmapped: &[],
};

pub static VIP9000_DEFAULT: Variant = Variant {
    driver: "vip9000",
    name: "default",
    needs: "vip_drv_os.h and vip_drv_share.h (vip2 dir)",
    layout: &["drivers/vip9000/src/layout.rs"],
    source: "allwinner BSP drivers/npu/aw_nna_vip/vip2",
    defines: &[
        "LINUX",
        "DRIVER",
        "USE_LINUX_PLATFORM_DEVICE",
        "NPU_USER_IOMMU=0",
        "DBG=0",
        "CONFIG_ARCH_SUN60IW2=1",
    ],
    preamble: r####"
#include <stddef.h>
#include <stdio.h>
#include <vip_drv_os.h>



#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))

"####,
    pairs: &[
        (
            "size_of::<IoctlData>()",
            r####"sizeof(struct ioctl_data)"####,
        ),
        (
            "offset_of!(IoctlData, command)",
            r####"offsetof(struct ioctl_data, command)"####,
        ),
        (
            "offset_of!(IoctlData, error)",
            r####"offsetof(struct ioctl_data, error)"####,
        ),
        (
            "offset_of!(IoctlData, bytes)",
            r####"offsetof(struct ioctl_data, bytes)"####,
        ),
        (
            "offset_of!(IoctlData, buffer)",
            r####"offsetof(struct ioctl_data, buffer)"####,
        ),
        (
            "size_of::<Initialize>()",
            r####"sizeof(vipdrv_initialize_t)"####,
        ),
        (
            "offset_of!(Initialize, video_mem_size)",
            r####"offsetof(vipdrv_initialize_t, video_mem_size)"####,
        ),
        (
            "offset_of!(Initialize, version)",
            r####"offsetof(vipdrv_initialize_t, version)"####,
        ),
        (
            "size_of::<Allocation>()",
            r####"sizeof(vipdrv_allocation_t)"####,
        ),
        (
            "offset_of!(Allocation, logical)",
            r####"offsetof(vipdrv_allocation_t, logical)"####,
        ),
        (
            "offset_of!(Allocation, mem_id)",
            r####"offsetof(vipdrv_allocation_t, mem_id)"####,
        ),
        (
            "offset_of!(Allocation, physical)",
            r####"offsetof(vipdrv_allocation_t, physical)"####,
        ),
        (
            "offset_of!(Allocation, size)",
            r####"offsetof(vipdrv_allocation_t, size)"####,
        ),
        (
            "offset_of!(Allocation, align)",
            r####"offsetof(vipdrv_allocation_t, align)"####,
        ),
        (
            "offset_of!(Allocation, alloc_flag)",
            r####"offsetof(vipdrv_allocation_t, alloc_flag)"####,
        ),
        (
            "offset_of!(Allocation, device_vis)",
            r####"offsetof(vipdrv_allocation_t, device_vis)"####,
        ),
        (
            "offset_of!(Allocation, specified)",
            r####"offsetof(vipdrv_allocation_t, specified)"####,
        ),
        (
            "size_of::<Deallocation>()",
            r####"sizeof(vipdrv_deallocation_t)"####,
        ),
        (
            "offset_of!(Deallocation, mem_id)",
            r####"offsetof(vipdrv_deallocation_t, mem_id)"####,
        ),
        (
            "size_of::<WrapUserFd>()",
            r####"sizeof(vipdrv_wrap_user_fd_t)"####,
        ),
        (
            "offset_of!(WrapUserFd, fd)",
            r####"offsetof(vipdrv_wrap_user_fd_t, fd)"####,
        ),
        (
            "offset_of!(WrapUserFd, memory_type)",
            r####"offsetof(vipdrv_wrap_user_fd_t, memory_type)"####,
        ),
        (
            "offset_of!(WrapUserFd, size)",
            r####"offsetof(vipdrv_wrap_user_fd_t, size)"####,
        ),
        (
            "offset_of!(WrapUserFd, device_vis)",
            r####"offsetof(vipdrv_wrap_user_fd_t, device_vis)"####,
        ),
        (
            "offset_of!(WrapUserFd, virtual_addr)",
            r####"offsetof(vipdrv_wrap_user_fd_t, virtual_addr)"####,
        ),
        (
            "offset_of!(WrapUserFd, mem_id)",
            r####"offsetof(vipdrv_wrap_user_fd_t, mem_id)"####,
        ),
        (
            "offset_of!(WrapUserFd, logical)",
            r####"offsetof(vipdrv_wrap_user_fd_t, logical)"####,
        ),
        ("size_of::<Reg>()", r####"sizeof(vipdrv_reg_t)"####),
        (
            "offset_of!(Reg, dev_index)",
            r####"offsetof(vipdrv_reg_t, dev_index)"####,
        ),
        (
            "offset_of!(Reg, core_index)",
            r####"offsetof(vipdrv_reg_t, core_index)"####,
        ),
        (
            "offset_of!(Reg, reg)",
            r####"offsetof(vipdrv_reg_t, reg)"####,
        ),
        (
            "offset_of!(Reg, data)",
            r####"offsetof(vipdrv_reg_t, data)"####,
        ),
        (
            "size_of::<CreateTask>()",
            r####"sizeof(vipdrv_create_task_param_t)"####,
        ),
        (
            "offset_of!(CreateTask, task_id)",
            r####"offsetof(vipdrv_create_task_param_t, task_id)"####,
        ),
        (
            "offset_of!(CreateTask, task_name)",
            r####"offsetof(vipdrv_create_task_param_t, task_name)"####,
        ),
        (
            "offset_of!(CreateTask, subtask_count)",
            r####"offsetof(vipdrv_create_task_param_t, subtask_count)"####,
        ),
        (
            "size_of::<DestroyTask>()",
            r####"sizeof(vipdrv_destroy_task_param_t)"####,
        ),
        (
            "offset_of!(DestroyTask, task_id)",
            r####"offsetof(vipdrv_destroy_task_param_t, task_id)"####,
        ),
        (
            "size_of::<SetTaskProperty>()",
            r####"sizeof(vipdrv_set_task_property_t)"####,
        ),
        (
            "offset_of!(SetTaskProperty, task_id)",
            r####"offsetof(vipdrv_set_task_property_t, task_id)"####,
        ),
        (
            "offset_of!(SetTaskProperty, kind)",
            r####"offsetof(vipdrv_set_task_property_t, type)"####,
        ),
        (
            "offset_of!(SetTaskProperty, index)",
            r####"offsetof(vipdrv_set_task_property_t, index)"####,
        ),
        (
            "offset_of!(SetTaskProperty, patch_type)",
            r####"offsetof(vipdrv_set_task_property_t, patch_type)"####,
        ),
        (
            "offset_of!(SetTaskProperty, param)",
            r####"offsetof(vipdrv_set_task_property_t, param)"####,
        ),
        (
            "size_of::<SubmitTask>()",
            r####"sizeof(vipdrv_submit_task_param_t)"####,
        ),
        (
            "offset_of!(SubmitTask, task_id)",
            r####"offsetof(vipdrv_submit_task_param_t, task_id)"####,
        ),
        (
            "offset_of!(SubmitTask, subtask_index)",
            r####"offsetof(vipdrv_submit_task_param_t, subtask_index)"####,
        ),
        (
            "offset_of!(SubmitTask, subtask_cnt)",
            r####"offsetof(vipdrv_submit_task_param_t, subtask_cnt)"####,
        ),
        (
            "offset_of!(SubmitTask, device_index)",
            r####"offsetof(vipdrv_submit_task_param_t, device_index)"####,
        ),
        (
            "offset_of!(SubmitTask, core_index)",
            r####"offsetof(vipdrv_submit_task_param_t, core_index)"####,
        ),
        (
            "offset_of!(SubmitTask, core_cnt)",
            r####"offsetof(vipdrv_submit_task_param_t, core_cnt)"####,
        ),
        (
            "offset_of!(SubmitTask, priority)",
            r####"offsetof(vipdrv_submit_task_param_t, priority)"####,
        ),
        (
            "offset_of!(SubmitTask, time_out)",
            r####"offsetof(vipdrv_submit_task_param_t, time_out)"####,
        ),
        (
            "offset_of!(SubmitTask, need_schedule)",
            r####"offsetof(vipdrv_submit_task_param_t, need_schedule)"####,
        ),
        (
            "size_of::<WaitTask>()",
            r####"sizeof(vipdrv_wait_task_param_t)"####,
        ),
        (
            "offset_of!(WaitTask, task_id)",
            r####"offsetof(vipdrv_wait_task_param_t, task_id)"####,
        ),
        (
            "offset_of!(WaitTask, subtask_index)",
            r####"offsetof(vipdrv_wait_task_param_t, subtask_index)"####,
        ),
        (
            "size_of::<WaitTask>()",
            r####"sizeof(vipdrv_cancel_task_param_t)"####,
        ),
        ("VIPDRV_IOCTL", r####"VIPDRV_IOCTL"####),
        ("MAX_CMD_SIZE", r####"vipdMAX_CMD_SIZE"####),
        ("APPEND_COMMAND_SIZE", r####"APPEND_COMMAND_SIZE"####),
        ("TASK_NAME_LENGTH", r####"TASK_NAME_LENGTH"####),
        (
            "Command::ReadReg as u32",
            r####"VIPDRV_INTRFACE_READ_REG"####,
        ),
        (
            "Command::WriteReg as u32",
            r####"VIPDRV_INTRFACE_WRITE_REG"####,
        ),
        (
            "Command::WaitTask as u32",
            r####"VIPDRV_INTRFACE_WAIT_TASK"####,
        ),
        (
            "Command::SubmitTask as u32",
            r####"VIPDRV_INTRFACE_SUBMIT_TASK"####,
        ),
        (
            "Command::Allocation as u32",
            r####"VIPDRV_INTRFACE_ALLOCATION"####,
        ),
        (
            "Command::Deallocation as u32",
            r####"VIPDRV_INTRFACE_DEALLOCATION"####,
        ),
        (
            "Command::WrapUserMemory as u32",
            r####"VIPDRV_INTRFACE_WRAP_USER_MEMORY"####,
        ),
        (
            "Command::WrapUserPhysical as u32",
            r####"VIPDRV_INTRFACE_WRAP_USER_PHYSICAL"####,
        ),
        (
            "Command::WrapUserFd as u32",
            r####"VIPDRV_INTRFACE_WRAP_USER_FD"####,
        ),
        (
            "Command::UnwrapUserFd as u32",
            r####"VIPDRV_INTRFACE_UNWRAP_USER_FD"####,
        ),
        (
            "Command::QueryMmuInfo as u32",
            r####"VIPDRV_INTRFACE_QUERY_MMU_INFO"####,
        ),
        (
            "Command::MapUserLogical as u32",
            r####"VIPDRV_INTRFACE_MAP_USER_LOGICAL"####,
        ),
        (
            "Command::CancelTask as u32",
            r####"VIPDRV_INTRFACE_CANCEL_TASK"####,
        ),
        (
            "Command::CreateTask as u32",
            r####"VIPDRV_INTRFACE_CREATE_TASK"####,
        ),
        (
            "Command::SetTaskProperty as u32",
            r####"VIPDRV_INTRFACE_SET_TASK_PROPERTY"####,
        ),
        (
            "Command::DestroyTask as u32",
            r####"VIPDRV_INTRFACE_DESTROY_TASK"####,
        ),
        ("Command::ALL.len()", r####"VIPDRV_INTRFACE_MAX"####),
        (
            "alloc_flag::CONTIGUOUS",
            r####"VIPDRV_VIDEO_MEM_ALLOC_CONTIGUOUS"####,
        ),
        (
            "alloc_flag::NO_MMU_PAGE",
            r####"VIPDRV_VIDEO_MEM_ALLOC_NO_MMU_PAGE"####,
        ),
        (
            "alloc_flag::MAP_USER",
            r####"VIPDRV_VIDEO_MEM_ALLOC_MAP_USER"####,
        ),
        (
            "alloc_flag::MMU_PAGE_MEM",
            r####"VIPDRV_VIDEO_MEM_ALLOC_MMU_PAGE_MEM"####,
        ),
        ("task_cmd::SUBTASK", r####"VIPDRV_TASK_CMD_SUBTASK"####),
        ("task_cmd::PRELOAD", r####"VIPDRV_TASK_CMD_PRELOAD"####),
        ("patch::NONE", r####"VIPDRV_TASK_PATCH_NONE"####),
        ("patch::DUP", r####"VIPDRV_TASK_PATCH_DUP"####),
    ],
    record: "unknown",
    unmapped: &[],
};

pub static RKISP_SYS_V510: Variant = Variant {
    driver: "rkisp-sys",
    name: "v510",
    needs: "linux/rk-isp2-config.h, rk-isp21-config.h, rk-isp3-config.h, rk-isp32-config.h (camera-module include cut)",
    layout: &[
        "drivers/rkisp-sys/src/layout.rs",
        "drivers/rkisp-sys/src/modules.rs",
    ],
    source: "rockchip-linux/kernel develop-5.10 include/uapi/linux",
    defines: &["V510"],
    preamble: r####"
#include <stddef.h>
#include <stdio.h>

#define BIT_ULL(n) (1ULL << (n))

#include <linux/rk-isp3-config.h>

"####,
    pairs: &[
        (
            "size_of::<isp3x_isp_params_cfg>()",
            r####"sizeof(struct isp3x_isp_params_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, module_en_update)",
            r####"offsetof(struct isp3x_isp_params_cfg, module_en_update)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, module_ens)",
            r####"offsetof(struct isp3x_isp_params_cfg, module_ens)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, module_cfg_update)",
            r####"offsetof(struct isp3x_isp_params_cfg, module_cfg_update)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, frame_id)",
            r####"offsetof(struct isp3x_isp_params_cfg, frame_id)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, meas)",
            r####"offsetof(struct isp3x_isp_params_cfg, meas)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, others)",
            r####"offsetof(struct isp3x_isp_params_cfg, others)"####,
        ),
        (
            "offset_of!(isp3x_isp_params_cfg, exposure)",
            r####"offsetof(struct isp3x_isp_params_cfg, exposure)"####,
        ),
        (
            "size_of::<isp3x_isp_meas_cfg>()",
            r####"sizeof(struct isp3x_isp_meas_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_meas_cfg, rawawb)",
            r####"offsetof(struct isp3x_isp_meas_cfg, rawawb)"####,
        ),
        (
            "offset_of!(isp3x_isp_meas_cfg, rawae0)",
            r####"offsetof(struct isp3x_isp_meas_cfg, rawae0)"####,
        ),
        (
            "offset_of!(isp3x_isp_meas_cfg, rawhist1)",
            r####"offsetof(struct isp3x_isp_meas_cfg, rawhist1)"####,
        ),
        (
            "size_of::<isp3x_isp_other_cfg>()",
            r####"sizeof(struct isp3x_isp_other_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_other_cfg, bls_cfg)",
            r####"offsetof(struct isp3x_isp_other_cfg, bls_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_other_cfg, awb_gain_cfg)",
            r####"offsetof(struct isp3x_isp_other_cfg, awb_gain_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_other_cfg, ccm_cfg)",
            r####"offsetof(struct isp3x_isp_other_cfg, ccm_cfg)"####,
        ),
        (
            "offset_of!(isp3x_isp_other_cfg, gammaout_cfg)",
            r####"offsetof(struct isp3x_isp_other_cfg, gammaout_cfg)"####,
        ),
        (
            "size_of::<isp3x_stat>()",
            r####"sizeof(struct isp3x_stat)"####,
        ),
        (
            "offset_of!(isp3x_stat, rawae0)",
            r####"offsetof(struct isp3x_stat, rawae0)"####,
        ),
        (
            "offset_of!(isp3x_stat, rawhist1)",
            r####"offsetof(struct isp3x_stat, rawhist1)"####,
        ),
        (
            "offset_of!(isp3x_stat, rawawb)",
            r####"offsetof(struct isp3x_stat, rawawb)"####,
        ),
        (
            "offset_of!(isp3x_stat, bls)",
            r####"offsetof(struct isp3x_stat, bls)"####,
        ),
        (
            "size_of::<rkisp3x_isp_stat_buffer>()",
            r####"sizeof(struct rkisp3x_isp_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp3x_isp_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp3x_isp_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp3x_isp_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp3x_isp_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp3x_isp_stat_buffer, params_id)",
            r####"offsetof(struct rkisp3x_isp_stat_buffer, params_id)"####,
        ),
        (
            "offset_of!(rkisp3x_isp_stat_buffer, params)",
            r####"offsetof(struct rkisp3x_isp_stat_buffer, params)"####,
        ),
        (
            "size_of::<isp21_isp_params_cfg>()",
            r####"sizeof(struct isp21_isp_params_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, module_en_update)",
            r####"offsetof(struct isp21_isp_params_cfg, module_en_update)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, module_ens)",
            r####"offsetof(struct isp21_isp_params_cfg, module_ens)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, module_cfg_update)",
            r####"offsetof(struct isp21_isp_params_cfg, module_cfg_update)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, frame_id)",
            r####"offsetof(struct isp21_isp_params_cfg, frame_id)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, meas)",
            r####"offsetof(struct isp21_isp_params_cfg, meas)"####,
        ),
        (
            "offset_of!(isp21_isp_params_cfg, others)",
            r####"offsetof(struct isp21_isp_params_cfg, others)"####,
        ),
        (
            "size_of::<isp21_isp_meas_cfg>()",
            r####"sizeof(struct isp21_isp_meas_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_meas_cfg, rawae3)",
            r####"offsetof(struct isp21_isp_meas_cfg, rawae3)"####,
        ),
        (
            "offset_of!(isp21_isp_meas_cfg, rawhist1)",
            r####"offsetof(struct isp21_isp_meas_cfg, rawhist1)"####,
        ),
        (
            "size_of::<isp21_isp_other_cfg>()",
            r####"sizeof(struct isp21_isp_other_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_other_cfg, bls_cfg)",
            r####"offsetof(struct isp21_isp_other_cfg, bls_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_other_cfg, lsc_cfg)",
            r####"offsetof(struct isp21_isp_other_cfg, lsc_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_other_cfg, awb_gain_cfg)",
            r####"offsetof(struct isp21_isp_other_cfg, awb_gain_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_other_cfg, ccm_cfg)",
            r####"offsetof(struct isp21_isp_other_cfg, ccm_cfg)"####,
        ),
        (
            "offset_of!(isp21_isp_other_cfg, gammaout_cfg)",
            r####"offsetof(struct isp21_isp_other_cfg, gammaout_cfg)"####,
        ),
        (
            "size_of::<isp2x_lsc_cfg>()",
            r####"sizeof(struct isp2x_lsc_cfg)"####,
        ),
        (
            "size_of::<isp2x_gammaout_cfg>()",
            r####"sizeof(struct isp2x_gammaout_cfg)"####,
        ),
        (
            "size_of::<isp21_stat>()",
            r####"sizeof(struct isp21_stat)"####,
        ),
        (
            "offset_of!(isp21_stat, rawae3)",
            r####"offsetof(struct isp21_stat, rawae3)"####,
        ),
        (
            "offset_of!(isp21_stat, rawhist1)",
            r####"offsetof(struct isp21_stat, rawhist1)"####,
        ),
        (
            "offset_of!(isp21_stat, bls)",
            r####"offsetof(struct isp21_stat, bls)"####,
        ),
        (
            "offset_of!(isp21_stat, dhaz)",
            r####"offsetof(struct isp21_stat, dhaz)"####,
        ),
        (
            "size_of::<rkisp_isp21_stat_buffer>()",
            r####"sizeof(struct rkisp_isp21_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp_isp21_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp_isp21_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp_isp21_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp_isp21_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp_isp21_stat_buffer, params_id)",
            r####"offsetof(struct rkisp_isp21_stat_buffer, params_id)"####,
        ),
        (
            "offset_of!(rkisp_isp21_stat_buffer, params)",
            r####"offsetof(struct rkisp_isp21_stat_buffer, params)"####,
        ),
        ("crate::module::DPCC", r####"ISP2X_MODULE_DPCC"####),
        ("crate::module::BLS", r####"ISP2X_MODULE_BLS"####),
        ("crate::module::SDG", r####"ISP2X_MODULE_SDG"####),
        ("crate::module::LSC", r####"ISP2X_MODULE_LSC"####),
        ("crate::module::AWB_GAIN", r####"ISP2X_MODULE_AWB_GAIN"####),
        ("crate::module::BDM", r####"ISP2X_MODULE_BDM"####),
        ("crate::module::CCM", r####"ISP2X_MODULE_CCM"####),
        ("crate::module::GOC", r####"ISP2X_MODULE_GOC"####),
        ("crate::module::CPROC", r####"ISP2X_MODULE_CPROC"####),
        ("crate::module::RAWAF", r####"ISP2X_MODULE_RAWAF"####),
        ("crate::module::RAWAE0", r####"ISP2X_MODULE_RAWAE0"####),
        ("crate::module::RAWAE1", r####"ISP2X_MODULE_RAWAE1"####),
        ("crate::module::RAWAE2", r####"ISP2X_MODULE_RAWAE2"####),
        ("crate::module::RAWAE3", r####"ISP2X_MODULE_RAWAE3"####),
        ("crate::module::RAWAWB", r####"ISP2X_MODULE_RAWAWB"####),
        ("crate::module::RAWHIST0", r####"ISP2X_MODULE_RAWHIST0"####),
        ("crate::module::RAWHIST1", r####"ISP2X_MODULE_RAWHIST1"####),
        ("crate::module::RAWHIST2", r####"ISP2X_MODULE_RAWHIST2"####),
        ("crate::module::RAWHIST3", r####"ISP2X_MODULE_RAWHIST3"####),
        ("crate::module::GIC", r####"ISP2X_MODULE_GIC"####),
        ("crate::module::DHAZ", r####"ISP2X_MODULE_DHAZ"####),
        ("crate::module::GAIN", r####"ISP2X_MODULE_GAIN"####),
        ("crate::module::DEBAYER", r####"ISP2X_MODULE_DEBAYER"####),
        ("crate::module::BAYNR", r####"ISP2X_MODULE_BAYNR"####),
        ("crate::module::BAY3D", r####"ISP2X_MODULE_BAY3D"####),
        ("crate::module::YNR", r####"ISP2X_MODULE_YNR"####),
        ("crate::module::CNR", r####"ISP2X_MODULE_CNR"####),
        ("crate::module::SHARP", r####"ISP2X_MODULE_SHARP"####),
        ("crate::module::DRC", r####"ISP2X_MODULE_DRC"####),
        ("crate::module::CSM", r####"ISP2X_MODULE_CSM"####),
        ("crate::module::CGC", r####"ISP2X_MODULE_CGC"####),
    ],
    record: "develop-5.10",
    unmapped: &[],
};

pub static RKISP_SYS_V61: Variant = Variant {
    driver: "rkisp-sys",
    name: "v61",
    needs: "linux/rk-isp2-config.h, rk-isp33-config.h, rk-isp35-config.h, rk-isp39-config.h",
    layout: &[
        "drivers/rkisp-sys/src/layout.rs",
        "drivers/rkisp-sys/src/modules.rs",
    ],
    source: "rockchip-linux/kernel develop-6.1 include/uapi/linux",
    defines: &["V61"],
    preamble: r####"
#include <stddef.h>
#include <stdio.h>

#define BIT_ULL(n) (1ULL << (n))


#include <linux/rk-isp35-config.h>

"####,
    pairs: &[
        (
            "size_of::<isp35_isp_params_cfg>()",
            r####"sizeof(struct isp35_isp_params_cfg)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, module_en_update)",
            r####"offsetof(struct isp35_isp_params_cfg, module_en_update)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, module_ens)",
            r####"offsetof(struct isp35_isp_params_cfg, module_ens)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, module_cfg_update)",
            r####"offsetof(struct isp35_isp_params_cfg, module_cfg_update)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, frame_id)",
            r####"offsetof(struct isp35_isp_params_cfg, frame_id)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, meas)",
            r####"offsetof(struct isp35_isp_params_cfg, meas)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, others)",
            r####"offsetof(struct isp35_isp_params_cfg, others)"####,
        ),
        (
            "offset_of!(isp35_isp_params_cfg, exposure)",
            r####"offsetof(struct isp35_isp_params_cfg, exposure)"####,
        ),
        (
            "size_of::<isp35_isp_meas_cfg>()",
            r####"sizeof(struct isp35_isp_meas_cfg)"####,
        ),
        (
            "size_of::<isp35_isp_other_cfg>()",
            r####"sizeof(struct isp35_isp_other_cfg)"####,
        ),
        (
            "offset_of!(isp35_isp_other_cfg, bls_cfg)",
            r####"offsetof(struct isp35_isp_other_cfg, bls_cfg)"####,
        ),
        (
            "offset_of!(isp35_isp_other_cfg, sharp_cfg)",
            r####"offsetof(struct isp35_isp_other_cfg, sharp_cfg)"####,
        ),
        (
            "offset_of!(isp35_isp_other_cfg, enh_cfg)",
            r####"offsetof(struct isp35_isp_other_cfg, enh_cfg)"####,
        ),
        (
            "size_of::<isp35_stat>()",
            r####"sizeof(struct isp35_stat)"####,
        ),
        (
            "offset_of!(isp35_stat, rawae3)",
            r####"offsetof(struct isp35_stat, rawae3)"####,
        ),
        (
            "offset_of!(isp35_stat, rawhist3)",
            r####"offsetof(struct isp35_stat, rawhist3)"####,
        ),
        (
            "offset_of!(isp35_stat, rawae0)",
            r####"offsetof(struct isp35_stat, rawae0)"####,
        ),
        (
            "offset_of!(isp35_stat, rawhist0)",
            r####"offsetof(struct isp35_stat, rawhist0)"####,
        ),
        (
            "offset_of!(isp35_stat, rawaf)",
            r####"offsetof(struct isp35_stat, rawaf)"####,
        ),
        (
            "offset_of!(isp35_stat, rawawb)",
            r####"offsetof(struct isp35_stat, rawawb)"####,
        ),
        (
            "offset_of!(isp35_stat, bay3d)",
            r####"offsetof(struct isp35_stat, bay3d)"####,
        ),
        (
            "offset_of!(isp35_stat, sharp)",
            r####"offsetof(struct isp35_stat, sharp)"####,
        ),
        (
            "offset_of!(isp35_stat, enh)",
            r####"offsetof(struct isp35_stat, enh)"####,
        ),
        (
            "offset_of!(isp35_stat, hist)",
            r####"offsetof(struct isp35_stat, hist)"####,
        ),
        (
            "size_of::<rkisp35_stat_buffer>()",
            r####"sizeof(struct rkisp35_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp35_stat_buffer, stat)",
            r####"offsetof(struct rkisp35_stat_buffer, stat)"####,
        ),
        (
            "offset_of!(rkisp35_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp35_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp35_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp35_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp35_stat_buffer, params_id)",
            r####"offsetof(struct rkisp35_stat_buffer, params_id)"####,
        ),
        (
            "size_of::<isp32_isp_params_cfg>()",
            r####"sizeof(struct isp32_isp_params_cfg)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, module_en_update)",
            r####"offsetof(struct isp32_isp_params_cfg, module_en_update)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, module_ens)",
            r####"offsetof(struct isp32_isp_params_cfg, module_ens)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, module_cfg_update)",
            r####"offsetof(struct isp32_isp_params_cfg, module_cfg_update)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, frame_id)",
            r####"offsetof(struct isp32_isp_params_cfg, frame_id)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, meas)",
            r####"offsetof(struct isp32_isp_params_cfg, meas)"####,
        ),
        (
            "offset_of!(isp32_isp_params_cfg, others)",
            r####"offsetof(struct isp32_isp_params_cfg, others)"####,
        ),
        (
            "size_of::<isp32_isp_meas_cfg>()",
            r####"sizeof(struct isp32_isp_meas_cfg)"####,
        ),
        (
            "size_of::<isp32_isp_other_cfg>()",
            r####"sizeof(struct isp32_isp_other_cfg)"####,
        ),
        (
            "size_of::<isp32_stat>()",
            r####"sizeof(struct isp32_stat)"####,
        ),
        (
            "size_of::<rkisp32_isp_stat_buffer>()",
            r####"sizeof(struct rkisp32_isp_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp32_isp_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp32_isp_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp32_isp_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp32_isp_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp32_isp_stat_buffer, params_id)",
            r####"offsetof(struct rkisp32_isp_stat_buffer, params_id)"####,
        ),
        (
            "offset_of!(rkisp32_isp_stat_buffer, params)",
            r####"offsetof(struct rkisp32_isp_stat_buffer, params)"####,
        ),
        (
            "size_of::<isp32_lite_stat>()",
            r####"sizeof(struct isp32_lite_stat)"####,
        ),
        (
            "size_of::<rkisp32_lite_stat_buffer>()",
            r####"sizeof(struct rkisp32_lite_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp32_lite_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp32_lite_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp32_lite_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp32_lite_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp32_lite_stat_buffer, params_id)",
            r####"offsetof(struct rkisp32_lite_stat_buffer, params_id)"####,
        ),
        (
            "offset_of!(rkisp32_lite_stat_buffer, params)",
            r####"offsetof(struct rkisp32_lite_stat_buffer, params)"####,
        ),
        (
            "size_of::<isp39_isp_params_cfg>()",
            r####"sizeof(struct isp39_isp_params_cfg)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, module_en_update)",
            r####"offsetof(struct isp39_isp_params_cfg, module_en_update)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, module_ens)",
            r####"offsetof(struct isp39_isp_params_cfg, module_ens)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, module_cfg_update)",
            r####"offsetof(struct isp39_isp_params_cfg, module_cfg_update)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, frame_id)",
            r####"offsetof(struct isp39_isp_params_cfg, frame_id)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, meas)",
            r####"offsetof(struct isp39_isp_params_cfg, meas)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, others)",
            r####"offsetof(struct isp39_isp_params_cfg, others)"####,
        ),
        (
            "offset_of!(isp39_isp_params_cfg, exposure)",
            r####"offsetof(struct isp39_isp_params_cfg, exposure)"####,
        ),
        (
            "size_of::<isp39_stat>()",
            r####"sizeof(struct isp39_stat)"####,
        ),
        (
            "size_of::<rkisp39_stat_buffer>()",
            r####"sizeof(struct rkisp39_stat_buffer)"####,
        ),
        (
            "offset_of!(rkisp39_stat_buffer, stat)",
            r####"offsetof(struct rkisp39_stat_buffer, stat)"####,
        ),
        (
            "offset_of!(rkisp39_stat_buffer, meas_type)",
            r####"offsetof(struct rkisp39_stat_buffer, meas_type)"####,
        ),
        (
            "offset_of!(rkisp39_stat_buffer, frame_id)",
            r####"offsetof(struct rkisp39_stat_buffer, frame_id)"####,
        ),
        (
            "offset_of!(rkisp39_stat_buffer, params_id)",
            r####"offsetof(struct rkisp39_stat_buffer, params_id)"####,
        ),
    ],
    record: "develop-6.1",
    unmapped: &[],
};

pub static PISP_FE: Variant = Variant {
    driver: "pisp",
    name: "fe",
    needs: "pisp_fe_config.h and pisp_statistics.h (rp1_cfe directory)",
    layout: &["drivers/pisp/src/uapi/layout.rs"],
    source: "Linux source drivers/media/platform/raspberrypi/rp1_cfe",
    defines: &["FE"],
    preamble: r####"
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#define OFF(c, r, f, rf) \
	printf("const _: () = assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(struct c, f))

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef int32_t s32;
typedef uint64_t u64;
#include "pisp_fe_config.h"
#include "pisp_statistics.h"


"####,
    pairs: &[
        (
            "size_of::<ImageFormatConfig>()",
            r####"sizeof(struct pisp_image_format_config)"####,
        ),
        (
            "size_of::<BlaConfig>()",
            r####"sizeof(struct pisp_bla_config)"####,
        ),
        (
            "size_of::<CompressConfig>()",
            r####"sizeof(struct pisp_compress_config)"####,
        ),
        (
            "size_of::<DecompressConfig>()",
            r####"sizeof(struct pisp_decompress_config)"####,
        ),
        (
            "size_of::<FeGlobalConfig>()",
            r####"sizeof(struct pisp_fe_global_config)"####,
        ),
        (
            "size_of::<FeInputConfig>()",
            r####"sizeof(struct pisp_fe_input_config)"####,
        ),
        (
            "size_of::<FeOutputConfig>()",
            r####"sizeof(struct pisp_fe_output_config)"####,
        ),
        (
            "size_of::<FeInputBufferConfig>()",
            r####"sizeof(struct pisp_fe_input_buffer_config)"####,
        ),
        (
            "size_of::<FeDecompandConfig>()",
            r####"sizeof(struct pisp_fe_decompand_config)"####,
        ),
        (
            "size_of::<FeDpcConfig>()",
            r####"sizeof(struct pisp_fe_dpc_config)"####,
        ),
        (
            "size_of::<FeLscConfig>()",
            r####"sizeof(struct pisp_fe_lsc_config)"####,
        ),
        (
            "size_of::<FeRgbyConfig>()",
            r####"sizeof(struct pisp_fe_rgby_config)"####,
        ),
        (
            "size_of::<FeAgcStatsConfig>()",
            r####"sizeof(struct pisp_fe_agc_stats_config)"####,
        ),
        (
            "offset_of!(FeAgcStatsConfig, row_offset_x)",
            r####"offsetof(struct pisp_fe_agc_stats_config, row_offset_x)"####,
        ),
        (
            "offset_of!(FeAgcStatsConfig, row_shift)",
            r####"offsetof(struct pisp_fe_agc_stats_config, row_shift)"####,
        ),
        (
            "size_of::<FeAwbStatsConfig>()",
            r####"sizeof(struct pisp_fe_awb_stats_config)"####,
        ),
        (
            "offset_of!(FeAwbStatsConfig, r_lo)",
            r####"offsetof(struct pisp_fe_awb_stats_config, r_lo)"####,
        ),
        (
            "size_of::<FeFloatingStatsConfig>()",
            r####"sizeof(struct pisp_fe_floating_stats_config)"####,
        ),
        (
            "size_of::<FeCdafStatsConfig>()",
            r####"sizeof(struct pisp_fe_cdaf_stats_config)"####,
        ),
        (
            "size_of::<FeCropConfig>()",
            r####"sizeof(struct pisp_fe_crop_config)"####,
        ),
        (
            "size_of::<FeDownscaleConfig>()",
            r####"sizeof(struct pisp_fe_downscale_config)"####,
        ),
        (
            "size_of::<FeOutputAxiConfig>()",
            r####"sizeof(struct pisp_fe_output_axi_config)"####,
        ),
        (
            "size_of::<FeOutputBranchConfig>()",
            r####"sizeof(struct pisp_fe_output_branch_config)"####,
        ),
        (
            "offset_of!(FeOutputBranchConfig, output)",
            r####"offsetof(struct pisp_fe_output_branch_config, output)"####,
        ),
        (
            "size_of::<FeConfig>()",
            r####"sizeof(struct pisp_fe_config)"####,
        ),
        (
            "offset_of!(FeConfig, stats_buffer)",
            r####"offsetof(struct pisp_fe_config, stats_buffer)"####,
        ),
        (
            "offset_of!(FeConfig, output_buffer)",
            r####"offsetof(struct pisp_fe_config, output_buffer)"####,
        ),
        (
            "offset_of!(FeConfig, input_buffer)",
            r####"offsetof(struct pisp_fe_config, input_buffer)"####,
        ),
        (
            "offset_of!(FeConfig, global)",
            r####"offsetof(struct pisp_fe_config, global)"####,
        ),
        (
            "offset_of!(FeConfig, input)",
            r####"offsetof(struct pisp_fe_config, input)"####,
        ),
        (
            "offset_of!(FeConfig, decompress)",
            r####"offsetof(struct pisp_fe_config, decompress)"####,
        ),
        (
            "offset_of!(FeConfig, decompand)",
            r####"offsetof(struct pisp_fe_config, decompand)"####,
        ),
        (
            "offset_of!(FeConfig, bla)",
            r####"offsetof(struct pisp_fe_config, bla)"####,
        ),
        (
            "offset_of!(FeConfig, dpc)",
            r####"offsetof(struct pisp_fe_config, dpc)"####,
        ),
        (
            "offset_of!(FeConfig, stats_crop)",
            r####"offsetof(struct pisp_fe_config, stats_crop)"####,
        ),
        (
            "offset_of!(FeConfig, spare1)",
            r####"offsetof(struct pisp_fe_config, spare1)"####,
        ),
        (
            "offset_of!(FeConfig, blc)",
            r####"offsetof(struct pisp_fe_config, blc)"####,
        ),
        (
            "offset_of!(FeConfig, rgby)",
            r####"offsetof(struct pisp_fe_config, rgby)"####,
        ),
        (
            "offset_of!(FeConfig, lsc)",
            r####"offsetof(struct pisp_fe_config, lsc)"####,
        ),
        (
            "offset_of!(FeConfig, agc_stats)",
            r####"offsetof(struct pisp_fe_config, agc_stats)"####,
        ),
        (
            "offset_of!(FeConfig, awb_stats)",
            r####"offsetof(struct pisp_fe_config, awb_stats)"####,
        ),
        (
            "offset_of!(FeConfig, cdaf_stats)",
            r####"offsetof(struct pisp_fe_config, cdaf_stats)"####,
        ),
        (
            "offset_of!(FeConfig, floating_stats)",
            r####"offsetof(struct pisp_fe_config, floating_stats)"####,
        ),
        (
            "offset_of!(FeConfig, output_axi)",
            r####"offsetof(struct pisp_fe_config, output_axi)"####,
        ),
        (
            "offset_of!(FeConfig, ch)",
            r####"offsetof(struct pisp_fe_config, ch)"####,
        ),
        (
            "offset_of!(FeConfig, dirty_flags)",
            r####"offsetof(struct pisp_fe_config, dirty_flags)"####,
        ),
        (
            "offset_of!(FeConfig, dirty_flags_extra)",
            r####"offsetof(struct pisp_fe_config, dirty_flags_extra)"####,
        ),
        (
            "size_of::<RawAgcZone>()",
            r####"sizeof(struct pisp_agc_statistics_zone)"####,
        ),
        (
            "size_of::<RawAgcStatistics>()",
            r####"sizeof(struct pisp_agc_statistics)"####,
        ),
        (
            "offset_of!(RawAgcStatistics, histogram)",
            r####"offsetof(struct pisp_agc_statistics, histogram)"####,
        ),
        (
            "offset_of!(RawAgcStatistics, floating)",
            r####"offsetof(struct pisp_agc_statistics, floating)"####,
        ),
        (
            "size_of::<RawAwbZone>()",
            r####"sizeof(struct pisp_awb_statistics_zone)"####,
        ),
        (
            "size_of::<RawAwbStatistics>()",
            r####"sizeof(struct pisp_awb_statistics)"####,
        ),
        (
            "size_of::<RawCdafStatistics>()",
            r####"sizeof(struct pisp_cdaf_statistics)"####,
        ),
        (
            "size_of::<RawStatistics>()",
            r####"sizeof(struct pisp_statistics)"####,
        ),
        (
            "offset_of!(RawStatistics, agc)",
            r####"offsetof(struct pisp_statistics, agc)"####,
        ),
        (
            "offset_of!(RawStatistics, cdaf)",
            r####"offsetof(struct pisp_statistics, cdaf)"####,
        ),
    ],
    record: "rpi-7.2.y",
    unmapped: &[],
};

pub static PISP_BE: Variant = Variant {
    driver: "pisp",
    name: "be",
    needs: "pisp_be_config.h (and pisp_common.h)",
    layout: &["drivers/pisp/src/uapi/layout.rs"],
    source: "Linux source include/uapi/linux/media/raspberrypi",
    defines: &["BE"],
    preamble: r####"
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#define OFF(c, r, f, rf) \
	printf("const _: () = assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(struct c, f))


#include "pisp_be_config.h"

"####,
    pairs: &[
        (
            "size_of::<WbgConfig>()",
            r####"sizeof(struct pisp_wbg_config)"####,
        ),
        (
            "size_of::<AxiConfig>()",
            r####"sizeof(struct pisp_axi_config)"####,
        ),
        (
            "size_of::<BeGlobalConfig>()",
            r####"sizeof(struct pisp_be_global_config)"####,
        ),
        (
            "size_of::<BeInputBufferConfig>()",
            r####"sizeof(struct pisp_be_input_buffer_config)"####,
        ),
        (
            "size_of::<BeDpcConfig>()",
            r####"sizeof(struct pisp_be_dpc_config)"####,
        ),
        (
            "size_of::<BeGeqConfig>()",
            r####"sizeof(struct pisp_be_geq_config)"####,
        ),
        (
            "size_of::<BeTdnConfig>()",
            r####"sizeof(struct pisp_be_tdn_config)"####,
        ),
        (
            "size_of::<BeSdnConfig>()",
            r####"sizeof(struct pisp_be_sdn_config)"####,
        ),
        (
            "size_of::<BeStitchConfig>()",
            r####"sizeof(struct pisp_be_stitch_config)"####,
        ),
        (
            "size_of::<BeCdnConfig>()",
            r####"sizeof(struct pisp_be_cdn_config)"####,
        ),
        (
            "size_of::<BeLscConfig>()",
            r####"sizeof(struct pisp_be_lsc_config)"####,
        ),
        (
            "size_of::<BeLscExtra>()",
            r####"sizeof(struct pisp_be_lsc_extra)"####,
        ),
        (
            "size_of::<BeCacConfig>()",
            r####"sizeof(struct pisp_be_cac_config)"####,
        ),
        (
            "size_of::<BeDebinConfig>()",
            r####"sizeof(struct pisp_be_debin_config)"####,
        ),
        (
            "size_of::<BeTonemapConfig>()",
            r####"sizeof(struct pisp_be_tonemap_config)"####,
        ),
        (
            "size_of::<BeDemosaicConfig>()",
            r####"sizeof(struct pisp_be_demosaic_config)"####,
        ),
        (
            "size_of::<BeCcmConfig>()",
            r####"sizeof(struct pisp_be_ccm_config)"####,
        ),
        (
            "offset_of!(BeCcmConfig, offsets)",
            r####"offsetof(struct pisp_be_ccm_config, offsets)"####,
        ),
        (
            "size_of::<BeSatControlConfig>()",
            r####"sizeof(struct pisp_be_sat_control_config)"####,
        ),
        (
            "size_of::<BeFalseColourConfig>()",
            r####"sizeof(struct pisp_be_false_colour_config)"####,
        ),
        (
            "size_of::<BeSharpenConfig>()",
            r####"sizeof(struct pisp_be_sharpen_config)"####,
        ),
        (
            "offset_of!(BeSharpenConfig, kernel4)",
            r####"offsetof(struct pisp_be_sharpen_config, kernel4)"####,
        ),
        (
            "offset_of!(BeSharpenConfig, thresholds)",
            r####"offsetof(struct pisp_be_sharpen_config, threshold_offset0)"####,
        ),
        (
            "offset_of!(BeSharpenConfig, positive_strength)",
            r####"offsetof(struct pisp_be_sharpen_config, positive_strength)"####,
        ),
        (
            "offset_of!(BeSharpenConfig, negative_limit)",
            r####"offsetof(struct pisp_be_sharpen_config, negative_limit)"####,
        ),
        (
            "offset_of!(BeSharpenConfig, enables)",
            r####"offsetof(struct pisp_be_sharpen_config, enables)"####,
        ),
        (
            "size_of::<BeShFcCombineConfig>()",
            r####"sizeof(struct pisp_be_sh_fc_combine_config)"####,
        ),
        (
            "size_of::<BeGammaConfig>()",
            r####"sizeof(struct pisp_be_gamma_config)"####,
        ),
        (
            "size_of::<BeCropConfig>()",
            r####"sizeof(struct pisp_be_crop_config)"####,
        ),
        (
            "size_of::<BeResampleConfig>()",
            r####"sizeof(struct pisp_be_resample_config)"####,
        ),
        (
            "size_of::<BeResampleExtra>()",
            r####"sizeof(struct pisp_be_resample_extra)"####,
        ),
        (
            "size_of::<BeDownscaleConfig>()",
            r####"sizeof(struct pisp_be_downscale_config)"####,
        ),
        (
            "size_of::<BeDownscaleExtra>()",
            r####"sizeof(struct pisp_be_downscale_extra)"####,
        ),
        (
            "size_of::<BeHogConfig>()",
            r####"sizeof(struct pisp_be_hog_config)"####,
        ),
        (
            "size_of::<BeAxiConfig>()",
            r####"sizeof(struct pisp_be_axi_config)"####,
        ),
        (
            "size_of::<BeOutputFormatConfig>()",
            r####"sizeof(struct pisp_be_output_format_config)"####,
        ),
        (
            "size_of::<BeConfig>()",
            r####"sizeof(struct pisp_be_config)"####,
        ),
        (
            "offset_of!(BeConfig, global)",
            r####"offsetof(struct pisp_be_config, global)"####,
        ),
        (
            "offset_of!(BeConfig, input_format)",
            r####"offsetof(struct pisp_be_config, input_format)"####,
        ),
        (
            "offset_of!(BeConfig, decompress)",
            r####"offsetof(struct pisp_be_config, decompress)"####,
        ),
        (
            "offset_of!(BeConfig, dpc)",
            r####"offsetof(struct pisp_be_config, dpc)"####,
        ),
        (
            "offset_of!(BeConfig, geq)",
            r####"offsetof(struct pisp_be_config, geq)"####,
        ),
        (
            "offset_of!(BeConfig, tdn_input_format)",
            r####"offsetof(struct pisp_be_config, tdn_input_format)"####,
        ),
        (
            "offset_of!(BeConfig, tdn_decompress)",
            r####"offsetof(struct pisp_be_config, tdn_decompress)"####,
        ),
        (
            "offset_of!(BeConfig, tdn)",
            r####"offsetof(struct pisp_be_config, tdn)"####,
        ),
        (
            "offset_of!(BeConfig, tdn_compress)",
            r####"offsetof(struct pisp_be_config, tdn_compress)"####,
        ),
        (
            "offset_of!(BeConfig, tdn_output_format)",
            r####"offsetof(struct pisp_be_config, tdn_output_format)"####,
        ),
        (
            "offset_of!(BeConfig, sdn)",
            r####"offsetof(struct pisp_be_config, sdn)"####,
        ),
        (
            "offset_of!(BeConfig, blc)",
            r####"offsetof(struct pisp_be_config, blc)"####,
        ),
        (
            "offset_of!(BeConfig, stitch_compress)",
            r####"offsetof(struct pisp_be_config, stitch_compress)"####,
        ),
        (
            "offset_of!(BeConfig, stitch_output_format)",
            r####"offsetof(struct pisp_be_config, stitch_output_format)"####,
        ),
        (
            "offset_of!(BeConfig, stitch_input_format)",
            r####"offsetof(struct pisp_be_config, stitch_input_format)"####,
        ),
        (
            "offset_of!(BeConfig, stitch_decompress)",
            r####"offsetof(struct pisp_be_config, stitch_decompress)"####,
        ),
        (
            "offset_of!(BeConfig, stitch)",
            r####"offsetof(struct pisp_be_config, stitch)"####,
        ),
        (
            "offset_of!(BeConfig, lsc)",
            r####"offsetof(struct pisp_be_config, lsc)"####,
        ),
        (
            "offset_of!(BeConfig, wbg)",
            r####"offsetof(struct pisp_be_config, wbg)"####,
        ),
        (
            "offset_of!(BeConfig, cdn)",
            r####"offsetof(struct pisp_be_config, cdn)"####,
        ),
        (
            "offset_of!(BeConfig, cac)",
            r####"offsetof(struct pisp_be_config, cac)"####,
        ),
        (
            "offset_of!(BeConfig, debin)",
            r####"offsetof(struct pisp_be_config, debin)"####,
        ),
        (
            "offset_of!(BeConfig, tonemap)",
            r####"offsetof(struct pisp_be_config, tonemap)"####,
        ),
        (
            "offset_of!(BeConfig, demosaic)",
            r####"offsetof(struct pisp_be_config, demosaic)"####,
        ),
        (
            "offset_of!(BeConfig, ccm)",
            r####"offsetof(struct pisp_be_config, ccm)"####,
        ),
        (
            "offset_of!(BeConfig, sat_control)",
            r####"offsetof(struct pisp_be_config, sat_control)"####,
        ),
        (
            "offset_of!(BeConfig, ycbcr)",
            r####"offsetof(struct pisp_be_config, ycbcr)"####,
        ),
        (
            "offset_of!(BeConfig, sharpen)",
            r####"offsetof(struct pisp_be_config, sharpen)"####,
        ),
        (
            "offset_of!(BeConfig, false_colour)",
            r####"offsetof(struct pisp_be_config, false_colour)"####,
        ),
        (
            "offset_of!(BeConfig, sh_fc_combine)",
            r####"offsetof(struct pisp_be_config, sh_fc_combine)"####,
        ),
        (
            "offset_of!(BeConfig, ycbcr_inverse)",
            r####"offsetof(struct pisp_be_config, ycbcr_inverse)"####,
        ),
        (
            "offset_of!(BeConfig, gamma)",
            r####"offsetof(struct pisp_be_config, gamma)"####,
        ),
        (
            "offset_of!(BeConfig, csc)",
            r####"offsetof(struct pisp_be_config, csc)"####,
        ),
        (
            "offset_of!(BeConfig, downscale)",
            r####"offsetof(struct pisp_be_config, downscale)"####,
        ),
        (
            "offset_of!(BeConfig, resample)",
            r####"offsetof(struct pisp_be_config, resample)"####,
        ),
        (
            "offset_of!(BeConfig, output_format)",
            r####"offsetof(struct pisp_be_config, output_format)"####,
        ),
        (
            "offset_of!(BeConfig, hog)",
            r####"offsetof(struct pisp_be_config, hog)"####,
        ),
        (
            "offset_of!(BeConfig, axi)",
            r####"offsetof(struct pisp_be_config, axi)"####,
        ),
        (
            "offset_of!(BeConfig, pad1)",
            r####"offsetof(struct pisp_be_config, pad1)"####,
        ),
        ("size_of::<Tile>()", r####"sizeof(struct pisp_tile)"####),
        (
            "offset_of!(Tile, input_addr_offset)",
            r####"offsetof(struct pisp_tile, input_addr_offset)"####,
        ),
        (
            "offset_of!(Tile, input_offset_x)",
            r####"offsetof(struct pisp_tile, input_offset_x)"####,
        ),
        (
            "offset_of!(Tile, tdn_input_addr_offset)",
            r####"offsetof(struct pisp_tile, tdn_input_addr_offset)"####,
        ),
        (
            "offset_of!(Tile, lsc_grid_offset_x)",
            r####"offsetof(struct pisp_tile, lsc_grid_offset_x)"####,
        ),
        (
            "offset_of!(Tile, cac_grid_offset_x)",
            r####"offsetof(struct pisp_tile, cac_grid_offset_x)"####,
        ),
        (
            "offset_of!(Tile, crop_x_start)",
            r####"offsetof(struct pisp_tile, crop_x_start)"####,
        ),
        (
            "offset_of!(Tile, crop_y_end)",
            r####"offsetof(struct pisp_tile, crop_y_end)"####,
        ),
        (
            "offset_of!(Tile, downscale_phase_x)",
            r####"offsetof(struct pisp_tile, downscale_phase_x)"####,
        ),
        (
            "offset_of!(Tile, downscale_phase_y)",
            r####"offsetof(struct pisp_tile, downscale_phase_y)"####,
        ),
        (
            "offset_of!(Tile, resample_in_width)",
            r####"offsetof(struct pisp_tile, resample_in_width)"####,
        ),
        (
            "offset_of!(Tile, resample_phase_x)",
            r####"offsetof(struct pisp_tile, resample_phase_x)"####,
        ),
        (
            "offset_of!(Tile, resample_phase_y)",
            r####"offsetof(struct pisp_tile, resample_phase_y)"####,
        ),
        (
            "offset_of!(Tile, output_offset_x)",
            r####"offsetof(struct pisp_tile, output_offset_x)"####,
        ),
        (
            "offset_of!(Tile, output_width)",
            r####"offsetof(struct pisp_tile, output_width)"####,
        ),
        (
            "offset_of!(Tile, output_height)",
            r####"offsetof(struct pisp_tile, output_height)"####,
        ),
        (
            "offset_of!(Tile, output_addr_offset)",
            r####"offsetof(struct pisp_tile, output_addr_offset)"####,
        ),
        (
            "offset_of!(Tile, output_addr_offset2)",
            r####"offsetof(struct pisp_tile, output_addr_offset2)"####,
        ),
        (
            "offset_of!(Tile, output_hog_addr_offset)",
            r####"offsetof(struct pisp_tile, output_hog_addr_offset)"####,
        ),
        (
            "size_of::<BeTilesConfig>()",
            r####"sizeof(struct pisp_be_tiles_config)"####,
        ),
        (
            "offset_of!(BeTilesConfig, tiles)",
            r####"offsetof(struct pisp_be_tiles_config, tiles)"####,
        ),
        (
            "offset_of!(BeTilesConfig, num_tiles)",
            r####"offsetof(struct pisp_be_tiles_config, num_tiles)"####,
        ),
    ],
    record: "rpi-7.2.y",
    unmapped: &[],
};

/// Every variant, in the order the tool lists them.
pub static VARIANTS: &[&Variant] = &[
    &ISP600_DEFAULT,
    &ISP600_SYS_DEFAULT,
    &RKAIISP_DEFAULT,
    &RKNPU_SYS_DEFAULT,
    &VIP9000_DEFAULT,
    &RKISP_SYS_V510,
    &RKISP_SYS_V61,
    &PISP_FE,
    &PISP_BE,
];
