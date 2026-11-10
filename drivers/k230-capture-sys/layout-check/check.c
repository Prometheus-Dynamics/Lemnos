/*
 * Checks the layouts and request numbers in drivers/k230-capture-sys/src/mipi.rs against the C
 * compiler. Not part of the crate build: ./run.sh compiles it for riscv64 (the board) and for the
 * host.
 *
 * The structures and requests are written out as vvcam_mipi.h declares them (k230_linux_sdk at
 * fc6cc21, buildroot-overlay/package/vvcam/mipi/), with the asm-generic _IOC encoding riscv uses.
 * Compiling is the check: every assertion is static. The vvcam-isp requests are checked by the
 * k230isp sys crate's own layout-check.
 */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define IOC(dir, type, nr, size) (((dir) << 30) | ((type) << 8) | (nr) | ((size) << 16))
#define IOW(type, nr, t) IOC(1u, (type), (nr), sizeof(t))

#define MIPI_LANE_NUM_MAX 8
#define MIPI_OUTPUT_INTERFACES_MAX 8

typedef enum { VVCAM_INPUT_MODE_MIPI = 0 } vvcam_input_mode_t;
typedef struct {
	unsigned int left;
	unsigned int top;
	unsigned int width;
	unsigned int height;
} vvcam_image_rect_t;
typedef enum { VVCAM_RAW_8BIT = 0, VVCAM_YUV422_10BIT = 18 } vvcam_data_type_t;
typedef enum { VVCAM_ITF_VCID_0 = 0, VVCAM_ITF_VCID_MAX = 8 } vvcam_itf_vcid_t;
typedef enum { VVCAM_ITF_BIT_MODE_48BIT = 0, VVCAM_ITF_BIT_MODE_16BIT } vvcam_itf_bit_mode_t;
typedef enum { VVCAM_SYNC_POL_HIGH_ACIVE = 0, VVCAM_SYNC_POL_LOW_ACIVE } vvcam_sync_pol_t;
typedef struct {
	uint32_t hsa;
	uint32_t hbp;
	uint32_t hfp;
	uint32_t hline;
} vvcam_itf_horizontal_time_t;
typedef struct {
	uint32_t vsa;
	uint32_t vbp;
	uint32_t vfp;
	uint32_t vactivate_lines;
} vvcam_itf_vertical_time_t;
typedef struct {
	vvcam_sync_pol_t pwdn;
	vvcam_sync_pol_t rst;
	vvcam_sync_pol_t fsin;
} vvcam_power_attr_t;
typedef struct {
	vvcam_power_attr_t powerPin;
	bool isPower;
} vvcam_dvp_dev_attr_t;
typedef struct {
	bool enable;
	bool embedded_data_en;
	vvcam_itf_vcid_t vcid;
	vvcam_itf_bit_mode_t bit_mode;
	vvcam_data_type_t data_type;
	vvcam_itf_horizontal_time_t h_time;
	vvcam_itf_vertical_time_t v_time;
} vvcam_mipi_output_itf_t;
typedef struct {
	vvcam_mipi_output_itf_t itf_cfg[MIPI_OUTPUT_INTERFACES_MAX];
	short lane_id[MIPI_LANE_NUM_MAX];
	uint32_t phy_freq;
} vvcam_mipi_dev_attr_t;
typedef struct {
	vvcam_input_mode_t mode;
	vvcam_image_rect_t image_rect;
	union {
		vvcam_mipi_dev_attr_t mipi_attr;
		vvcam_dvp_dev_attr_t dvp_attr;
	};
} vvcam_input_dev_attr_t;

_Static_assert(sizeof(vvcam_mipi_output_itf_t) == 48, "Interface");
_Static_assert(offsetof(vvcam_mipi_output_itf_t, vcid) == 4, "Interface.vcid");
_Static_assert(offsetof(vvcam_mipi_output_itf_t, bit_mode) == 8, "Interface.bit_mode");
_Static_assert(offsetof(vvcam_mipi_output_itf_t, data_type) == 12, "Interface.data_type");
_Static_assert(offsetof(vvcam_mipi_output_itf_t, h_time) == 16, "Interface.h_time");
_Static_assert(offsetof(vvcam_mipi_output_itf_t, v_time) == 32, "Interface.v_time");
_Static_assert(sizeof(vvcam_mipi_dev_attr_t) == 404, "MipiAttr");
_Static_assert(offsetof(vvcam_mipi_dev_attr_t, lane_id) == 384, "MipiAttr.lane_id");
_Static_assert(offsetof(vvcam_mipi_dev_attr_t, phy_freq) == 400, "MipiAttr.phy_freq");
_Static_assert(sizeof(vvcam_input_dev_attr_t) == 424, "DevAttr");
_Static_assert(offsetof(vvcam_input_dev_attr_t, image_rect) == 4, "DevAttr.image_rect");
_Static_assert(offsetof(vvcam_input_dev_attr_t, mipi_attr) == 20, "DevAttr.mipi");
_Static_assert(IOW('v', 0x01, unsigned int) == 0x40047601u, "RESET");
_Static_assert(IOW('v', 0x02, vvcam_input_dev_attr_t) == 0x41A87602u, "SET_DEV_ATTR");
_Static_assert(IOW('v', 0x03, unsigned int) == 0x40047603u, "SET_STREAM");
