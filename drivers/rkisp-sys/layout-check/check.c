/*
 * Generates the layout assertions in drivers/rkisp-sys/src/layout.rs from the vendor kernel's ISP
 * uAPI headers. Not part of the crate build: run ./run.sh <develop-5.10 dir> <develop-6.1 dir>
 * and it diffs the result against the file.
 *
 * Built twice: -DV510 against the develop-5.10 headers (ISP30's rk-isp3-config.h, which includes
 * ISP21's) and -DV61 against the develop-6.1 headers laid over them (ISP35's rk-isp35-config.h).
 */
#include <stddef.h>
#include <stdio.h>

#define SIZE(s) printf("    assert!(size_of::<%s>() == %zu);\n", #s, sizeof(struct s))
#define OFF(s, f) printf("    assert!(offset_of!(%s, %s) == %zu);\n", #s, #f, offsetof(struct s, f))
#define BEGIN(m) printf("const _: () = {\n    use crate::%s::*;\n", m)
#define END() printf("};\n")
#define MOD(n) printf("    assert!(crate::module::%s == %#llx);\n", #n, (unsigned long long)ISP2X_MODULE_##n)
#ifndef BIT_ULL
#define BIT_ULL(n) (1ULL << (n))
#endif

#ifdef V510
#include <linux/rk-isp3-config.h>

int main(void)
{
	printf("// ISP30 (develop-5.10 rk-isp3-config.h).\n");
	BEGIN("isp3x");
	SIZE(isp3x_isp_params_cfg);
	OFF(isp3x_isp_params_cfg, module_en_update);
	OFF(isp3x_isp_params_cfg, module_ens);
	OFF(isp3x_isp_params_cfg, module_cfg_update);
	OFF(isp3x_isp_params_cfg, frame_id);
	OFF(isp3x_isp_params_cfg, meas);
	OFF(isp3x_isp_params_cfg, others);
	OFF(isp3x_isp_params_cfg, exposure);
	SIZE(isp3x_isp_meas_cfg);
	OFF(isp3x_isp_meas_cfg, rawawb);
	OFF(isp3x_isp_meas_cfg, rawae0);
	OFF(isp3x_isp_meas_cfg, rawhist1);
	SIZE(isp3x_isp_other_cfg);
	OFF(isp3x_isp_other_cfg, bls_cfg);
	OFF(isp3x_isp_other_cfg, awb_gain_cfg);
	OFF(isp3x_isp_other_cfg, ccm_cfg);
	OFF(isp3x_isp_other_cfg, gammaout_cfg);
	SIZE(isp3x_stat);
	OFF(isp3x_stat, rawae0);
	OFF(isp3x_stat, rawhist1);
	OFF(isp3x_stat, rawawb);
	OFF(isp3x_stat, bls);
	SIZE(rkisp3x_isp_stat_buffer);
	OFF(rkisp3x_isp_stat_buffer, meas_type);
	OFF(rkisp3x_isp_stat_buffer, frame_id);
	OFF(rkisp3x_isp_stat_buffer, params_id);
	OFF(rkisp3x_isp_stat_buffer, params);
	END();

	printf("// ISP21 (develop-5.10 rk-isp21-config.h, rk-isp2-config.h).\n");
	BEGIN("isp21");
	SIZE(isp21_isp_params_cfg);
	OFF(isp21_isp_params_cfg, module_en_update);
	OFF(isp21_isp_params_cfg, module_ens);
	OFF(isp21_isp_params_cfg, module_cfg_update);
	OFF(isp21_isp_params_cfg, frame_id);
	OFF(isp21_isp_params_cfg, meas);
	OFF(isp21_isp_params_cfg, others);
	SIZE(isp21_isp_meas_cfg);
	OFF(isp21_isp_meas_cfg, rawae3);
	OFF(isp21_isp_meas_cfg, rawhist1);
	SIZE(isp21_isp_other_cfg);
	OFF(isp21_isp_other_cfg, bls_cfg);
	OFF(isp21_isp_other_cfg, lsc_cfg);
	OFF(isp21_isp_other_cfg, awb_gain_cfg);
	OFF(isp21_isp_other_cfg, ccm_cfg);
	OFF(isp21_isp_other_cfg, gammaout_cfg);
	SIZE(isp2x_lsc_cfg);
	SIZE(isp2x_gammaout_cfg);
	SIZE(isp21_stat);
	OFF(isp21_stat, rawae3);
	OFF(isp21_stat, rawhist1);
	OFF(isp21_stat, bls);
	OFF(isp21_stat, dhaz);
	SIZE(rkisp_isp21_stat_buffer);
	OFF(rkisp_isp21_stat_buffer, meas_type);
	OFF(rkisp_isp21_stat_buffer, frame_id);
	OFF(rkisp_isp21_stat_buffer, params_id);
	OFF(rkisp_isp21_stat_buffer, params);
	END();

	printf("// ISP21 and ISP30 module bits (ISP2X_MODULE_*).\n");
	printf("const _: () = {\n");
	MOD(DPCC);
	MOD(BLS);
	MOD(SDG);
	MOD(LSC);
	MOD(AWB_GAIN);
	MOD(BDM);
	MOD(CCM);
	MOD(GOC);
	MOD(CPROC);
	MOD(RAWAF);
	MOD(RAWAE0);
	MOD(RAWAE1);
	MOD(RAWAE2);
	MOD(RAWAE3);
	MOD(RAWAWB);
	MOD(RAWHIST0);
	MOD(RAWHIST1);
	MOD(RAWHIST2);
	MOD(RAWHIST3);
	MOD(GIC);
	MOD(DHAZ);
	MOD(GAIN);
	MOD(DEBAYER);
	MOD(BAYNR);
	MOD(BAY3D);
	MOD(YNR);
	MOD(CNR);
	MOD(SHARP);
	MOD(DRC);
	MOD(CSM);
	MOD(CGC);
	END();
	return 0;
}
#endif

#ifdef V61
#include <linux/rk-isp35-config.h>

int main(void)
{
	printf("// ISP35 (develop-6.1 rk-isp35-config.h).\n");
	BEGIN("isp35");
	SIZE(isp35_isp_params_cfg);
	OFF(isp35_isp_params_cfg, module_en_update);
	OFF(isp35_isp_params_cfg, module_ens);
	OFF(isp35_isp_params_cfg, module_cfg_update);
	OFF(isp35_isp_params_cfg, frame_id);
	OFF(isp35_isp_params_cfg, meas);
	OFF(isp35_isp_params_cfg, others);
	OFF(isp35_isp_params_cfg, exposure);
	SIZE(isp35_isp_meas_cfg);
	SIZE(isp35_isp_other_cfg);
	OFF(isp35_isp_other_cfg, bls_cfg);
	OFF(isp35_isp_other_cfg, sharp_cfg);
	OFF(isp35_isp_other_cfg, enh_cfg);
	SIZE(isp35_stat);
	OFF(isp35_stat, rawae3);
	OFF(isp35_stat, rawhist3);
	OFF(isp35_stat, rawae0);
	OFF(isp35_stat, rawhist0);
	OFF(isp35_stat, rawaf);
	OFF(isp35_stat, rawawb);
	OFF(isp35_stat, bay3d);
	OFF(isp35_stat, sharp);
	OFF(isp35_stat, enh);
	OFF(isp35_stat, hist);
	SIZE(rkisp35_stat_buffer);
	OFF(rkisp35_stat_buffer, stat);
	OFF(rkisp35_stat_buffer, meas_type);
	OFF(rkisp35_stat_buffer, frame_id);
	OFF(rkisp35_stat_buffer, params_id);
	END();

	printf("// ISP32 (develop-6.1 rk-isp32-config.h, lite statistics included).\n");
	BEGIN("isp32");
	SIZE(isp32_isp_params_cfg);
	OFF(isp32_isp_params_cfg, module_en_update);
	OFF(isp32_isp_params_cfg, module_ens);
	OFF(isp32_isp_params_cfg, module_cfg_update);
	OFF(isp32_isp_params_cfg, frame_id);
	OFF(isp32_isp_params_cfg, meas);
	OFF(isp32_isp_params_cfg, others);
	SIZE(isp32_isp_meas_cfg);
	SIZE(isp32_isp_other_cfg);
	SIZE(isp32_stat);
	SIZE(rkisp32_isp_stat_buffer);
	OFF(rkisp32_isp_stat_buffer, meas_type);
	OFF(rkisp32_isp_stat_buffer, frame_id);
	OFF(rkisp32_isp_stat_buffer, params_id);
	OFF(rkisp32_isp_stat_buffer, params);
	SIZE(isp32_lite_stat);
	SIZE(rkisp32_lite_stat_buffer);
	OFF(rkisp32_lite_stat_buffer, meas_type);
	OFF(rkisp32_lite_stat_buffer, frame_id);
	OFF(rkisp32_lite_stat_buffer, params_id);
	OFF(rkisp32_lite_stat_buffer, params);
	END();

	printf("// ISP39 (develop-6.1 rk-isp39-config.h).\n");
	BEGIN("isp39");
	SIZE(isp39_isp_params_cfg);
	OFF(isp39_isp_params_cfg, module_en_update);
	OFF(isp39_isp_params_cfg, module_ens);
	OFF(isp39_isp_params_cfg, module_cfg_update);
	OFF(isp39_isp_params_cfg, frame_id);
	OFF(isp39_isp_params_cfg, meas);
	OFF(isp39_isp_params_cfg, others);
	OFF(isp39_isp_params_cfg, exposure);
	SIZE(isp39_stat);
	SIZE(rkisp39_stat_buffer);
	OFF(rkisp39_stat_buffer, stat);
	OFF(rkisp39_stat_buffer, meas_type);
	OFF(rkisp39_stat_buffer, frame_id);
	OFF(rkisp39_stat_buffer, params_id);
	END();
	return 0;
}
#endif
