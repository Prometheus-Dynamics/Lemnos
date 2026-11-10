/*
 * Generates the layout assertions in drivers/rkaiisp/src/layout.rs from the vendor kernel's
 * AI-ISP uAPI headers. Not part of the crate build: run ./run.sh <include/uapi/linux dir> and it
 * diffs the result against the file.
 *
 * The headers are rk-aiisp-config.h and rk-isp2-config.h from rockchip-linux/kernel develop-6.1
 * at 77168c8d5ab82399f65a80e9f807b50ba37cf483. Every structure is packed, so the numbers hold on
 * aarch64 and x86-64 alike.
 */
#include <stddef.h>
#include <stdio.h>
#include <linux/videodev2.h>
#include <linux/rk-aiisp-config.h>

/* Rust type, C type. */
#define SIZE(r, c) printf("    assert!(size_of::<%s>() == %zu);\n", #r, sizeof(c))
/* Rust type and field, C type and member. */
#define OFF(r, rf, c, cf) \
	printf("    assert!(offset_of!(%s, %s) == %zu);\n", #r, #rf, offsetof(c, cf))
#define NUM(r, c) printf("    assert!(%s == %#llx);\n", #r, (unsigned long long)(c))

int main(void)
{
	printf("// Structures (rk-aiisp-config.h, rk-isp2-config.h).\n");
	printf("const _: () = {\n");
	SIZE(ParamInfo, struct rkaiisp_param_info);
	OFF(ParamInfo, exemode, struct rkaiisp_param_info, exemode);
	OFF(ParamInfo, para_size, struct rkaiisp_param_info, para_size);
	OFF(ParamInfo, max_runcnt, struct rkaiisp_param_info, max_runcnt);

	SIZE(AirmsQueue, struct rkaiisp_airms_st);
	OFF(AirmsQueue, inbuf_idx, struct rkaiisp_airms_st, inbuf_idx);
	OFF(AirmsQueue, outbuf_idx, struct rkaiisp_airms_st, outbuf_idx);
	SIZE(AibnrQueue, struct rkisp_aiisp_st);
	OFF(AibnrQueue, sequence, struct rkisp_aiisp_st, sequence);
	OFF(AibnrQueue, iir_index, struct rkisp_aiisp_st, iir_index);
	OFF(AibnrQueue, gain_index, struct rkisp_aiisp_st, gain_index);
	OFF(AibnrQueue, aiisp_index, struct rkisp_aiisp_st, aiisp_index);
	OFF(AibnrQueue, aipre_gain_index, struct rkisp_aiisp_st, aipre_gain_index);
	OFF(AibnrQueue, vpsl_index, struct rkisp_aiisp_st, vpsl_index);
	OFF(AibnrQueue, y_src_index, struct rkisp_aiisp_st, y_src_index);
	OFF(AibnrQueue, y_dest_index, struct rkisp_aiisp_st, y_dest_index);
	NUM(QUEUE_BUF_BYTES, sizeof(union rkaiisp_queue_buf));
	OFF(AibnrQueue, sequence, union rkaiisp_queue_buf, aibnr_st.sequence);
	OFF(AirmsQueue, sequence, union rkaiisp_queue_buf, airms_st.sequence);

	SIZE(IspBuf, struct rkisp_buf_info);
	OFF(IspBuf, buf_size, struct rkisp_buf_info, buf_size);
	OFF(IspBuf, buf_stride, struct rkisp_buf_info, buf_stride);
	OFF(IspBuf, buf_fd, struct rkisp_buf_info, buf_fd);
	SIZE(BnrBufInfo, struct rkisp_bnr_buf_info);
	OFF(BnrBufInfo, ds, struct rkisp_bnr_buf_info, u.v35.ds);
	OFF(BnrBufInfo, wgt, struct rkisp_bnr_buf_info, u.v35.wgt);
	OFF(BnrBufInfo, aiisp, struct rkisp_bnr_buf_info, u.v35.aiisp);
	OFF(BnrBufInfo, gain, struct rkisp_bnr_buf_info, u.v35.gain);
	OFF(BnrBufInfo, aipre_gain, struct rkisp_bnr_buf_info, u.v35.aipre_gain);
	OFF(BnrBufInfo, vpsl, struct rkisp_bnr_buf_info, u.v35.vpsl);
	OFF(BnrBufInfo, y_src, struct rkisp_bnr_buf_info, u.v35.y_src);
	OFF(BnrBufInfo, iir_rw_fmt, struct rkisp_bnr_buf_info, u.v35.iir_rw_fmt);
	OFF(BnrBufInfo, aibnr_l2, struct rkisp_bnr_buf_info, u.v35.aibnr_l2);
	OFF(BnrBufInfo, vpsl_yraw_offs, struct rkisp_bnr_buf_info, u.v35.vpsl_yraw_offs);
	OFF(BnrBufInfo, vpsl_yraw_stride, struct rkisp_bnr_buf_info, u.v35.vpsl_yraw_stride);
	OFF(BnrBufInfo, vpsl_sig_offs, struct rkisp_bnr_buf_info, u.v35.vpsl_sig_offs);
	OFF(BnrBufInfo, vpsl_sig_stride, struct rkisp_bnr_buf_info, u.v35.vpsl_sig_stride);
	SIZE(IspbufInfo, struct rkaiisp_ispbuf_info);
	OFF(IspbufInfo, iir_width, struct rkaiisp_ispbuf_info, iir_width);
	OFF(IspbufInfo, raw_width, struct rkaiisp_ispbuf_info, raw_width);
	OFF(IspbufInfo, raw_height, struct rkaiisp_ispbuf_info, raw_height);
	OFF(IspbufInfo, sig_width, struct rkaiisp_ispbuf_info, sig_width);
	OFF(IspbufInfo, sig_height, struct rkaiisp_ispbuf_info, sig_height);
	OFF(IspbufInfo, narmap_width, struct rkaiisp_ispbuf_info, narmap_width);
	OFF(IspbufInfo, narmap_height, struct rkaiisp_ispbuf_info, narmap_height);

	SIZE(RmsbufInfo, struct rkaiisp_rmsbuf_info);
	OFF(RmsbufInfo, inbuf_num, struct rkaiisp_rmsbuf_info, inbuf_num);
	OFF(RmsbufInfo, inbuf_fd, struct rkaiisp_rmsbuf_info, inbuf_fd);
	OFF(RmsbufInfo, outbuf_fd, struct rkaiisp_rmsbuf_info, outbuf_fd);
	SIZE(YnrbufInfo, struct rkaiisp_ynrbuf_info);
	OFF(YnrbufInfo, buf_cnt, struct rkaiisp_ynrbuf_info, buf_cnt);
	OFF(YnrbufInfo, dma_fd, struct rkaiisp_ynrbuf_info, dma_fd);

	SIZE(OtherCfg, struct rkaiisp_other_cfg);
	OFF(OtherCfg, sw_post_blacklvl, struct rkaiisp_other_cfg, sw_post_blacklvl);
	OFF(OtherCfg, sw_in_comp_y, struct rkaiisp_other_cfg, sw_in_comp_y);
	OFF(OtherCfg, sw_out_decomp_y, struct rkaiisp_other_cfg, sw_out_decomp_y);
	SIZE(ModelCfg, struct rkaiisp_model_cfg);
	OFF(ModelCfg, sw_aiisp_mode, struct rkaiisp_model_cfg, sw_aiisp_mode);
	OFF(ModelCfg, sw_aiisp_level_num, struct rkaiisp_model_cfg, sw_aiisp_level_num);
	OFF(ModelCfg, sw_aiisp_l1_level_num, struct rkaiisp_model_cfg, sw_aiisp_l1_level_num);
	OFF(ModelCfg, sw_aiisp_op_mode, struct rkaiisp_model_cfg, sw_aiisp_op_mode);
	OFF(ModelCfg, sw_aiisp_drop_en, struct rkaiisp_model_cfg, sw_aiisp_drop_en);
	OFF(ModelCfg, sw_aiisp_lv_active, struct rkaiisp_model_cfg, sw_aiisp_lv_active);
	OFF(ModelCfg, sw_aiisp_lv_mode, struct rkaiisp_model_cfg, sw_aiisp_lv_mode);
	OFF(ModelCfg, sw_mi_chn_en, struct rkaiisp_model_cfg, sw_mi_chn_en);
	OFF(ModelCfg, sw_mi_chn_mode, struct rkaiisp_model_cfg, sw_mi_chn_mode);
	OFF(ModelCfg, sw_mi_chn_num, struct rkaiisp_model_cfg, sw_mi_chn_num);
	OFF(ModelCfg, sw_mi_chn_data_mode, struct rkaiisp_model_cfg, sw_mi_chn_data_mode);
	OFF(ModelCfg, sw_mi_chn1_sel, struct rkaiisp_model_cfg, sw_mi_chn1_sel);
	OFF(ModelCfg, sw_mi_chn3_sel, struct rkaiisp_model_cfg, sw_mi_chn3_sel);
	OFF(ModelCfg, sw_out_d2s_en, struct rkaiisp_model_cfg, sw_out_d2s_en);
	OFF(ModelCfg, sw_out_mode, struct rkaiisp_model_cfg, sw_out_mode);
	OFF(ModelCfg, sw_lastlvlm1_clip8bit, struct rkaiisp_model_cfg, sw_lastlvlm1_clip8bit);
	SIZE(KwtCfg, struct rkaiisp_kwt_cfg);
	OFF(KwtCfg, kwt_size, struct rkaiisp_kwt_cfg, kwt_size);
	OFF(KwtCfg, kwt_pad_size, struct rkaiisp_kwt_cfg, kwt_pad_size);
	SIZE(Params, struct rkaiisp_params);
	OFF(Params, module_update, struct rkaiisp_params, module_update);
	OFF(Params, model_runcnt, struct rkaiisp_params, model_runcnt);
	OFF(Params, model_mode, struct rkaiisp_params, model_mode);
	OFF(Params, other_cfg, struct rkaiisp_params, other_cfg);
	OFF(Params, model_cfg, struct rkaiisp_params, model_cfg);
	OFF(Params, kwt_cfg, struct rkaiisp_params, kwt_cfg);
	OFF(Params, reserved, struct rkaiisp_params, reserved);
	SIZE(ModelInfo, struct rkaiisp_model_info);
	OFF(ModelInfo, model_qr, struct rkaiisp_model_info, model_qr);
	OFF(ModelInfo, model_mode, struct rkaiisp_model_info, model_mode);
	OFF(ModelInfo, model_cfg, struct rkaiisp_model_info, model_cfg);
	OFF(ModelInfo, kwt_cfg, struct rkaiisp_model_info, kwt_cfg);
	OFF(ModelInfo, reserved, struct rkaiisp_model_info, reserved);
	printf("};\n");

	printf("// Requests, the event and the constants.\n");
	printf("const _: () = {\n");
	NUM(CMD_SET_PARAM_INFO, RKAIISP_CMD_SET_PARAM_INFO);
	NUM(CMD_INIT_BUFPOOL, RKAIISP_CMD_INIT_BUFPOOL);
	NUM(CMD_FREE_BUFPOOL, RKAIISP_CMD_FREE_BUFPOOL);
	NUM(CMD_QUEUE_BUF, RKAIISP_CMD_QUEUE_BUF);
	NUM(CMD_INIT_AIRMS_BUFPOOL, RKAIISP_CMD_INIT_AIRMS_BUFPOOL);
	NUM(CMD_GET_YNRBUF_INFO, RKAIISP_CMD_GET_YNRBUF_INFO);
	NUM(CMD_SET_MEMORY_MODE, RKAIISP_CMD_SET_MEMORY_MODE);
	NUM(CMD_CLEAR_IQPARAMS, RKAIISP_CMD_CLEAR_IQPARAMS);
	NUM(EVENT_AIISP_DONE, RKAIISP_V4L2_EVENT_AIISP_DONE);
	NUM(META_FORMAT, V4L2_META_FMT_RK_ISP1_PARAMS);
	NUM(MAX_RUNCNT, RKAIISP_MAX_RUNCNT);
	NUM(MAX_ISPBUF, RKAIISP_MAX_ISPBUF);
	NUM(MODEL_UPDATE, RKAIISP_MODEL_UPDATE);
	NUM(OTHER_UPDATE, RKAIISP_OTHER_UPDATE);
	NUM(AIYNR_YBUF_NUM_MAX, RKAIISP_AIYNR_YBUF_NUM_MAX);
	NUM(ISP_BUFFER_MAX, RKISP_BUFFER_MAX);
	NUM(VPSL_YRAW_CHN_MAX, VPSL_YRAW_CHN_MAX);
	NUM(VPSL_SIG_CHN_MAX, VPSL_SIG_CHN_MAX);
	NUM(chn_src::ISP_IIR, ISP_IIR);
	NUM(chn_src::ISP_AIPRE_NARMAP, ISP_AIPRE_NARMAP);
	NUM(chn_src::AIISP_LAST_OUT, AIISP_LAST_OUT);
	NUM(chn_src::VICAP_BAYER_RAW_DOWN, VICAP_BAYER_RAW_DOWN);
	NUM(exealgo::AIYNR, AIYNR);
	NUM(model_mode::COMBO_MODE, COMBO_MODE);
	NUM(model_mode::AIYNR_MODE, AIYNR_MODE);
	NUM(exemode::BOTHEVENT_IN_KERNEL, BOTHEVENT_IN_KERNEL);
	NUM(mem_mode::COMBO_MEMODE, COMBO_MEMODE);
	printf("};\n");
	return 0;
}
