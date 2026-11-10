/*
 * The OSD and border controls of the K230 SDK's nonai-2d driver, as its header declares them
 * (k230_linux_sdk at fc6cc2151d54cb307be47f81815023f7ffb0eae6,
 * buildroot-overlay/package/nonai2d/src/nonai2d-v4l2-controls.h), checked against the offsets
 * src/overlay.rs writes. Only the declarations are transcribed; nothing here runs.
 */
#include <stddef.h>
#include <stdint.h>

#define OSD_REGION_NUM 8
#define BORDER_NUM 32
#define CID_USER_BASE 0x00980900u
#define CID_NONAI2D_BASE (CID_USER_BASE + 0x1200u)

typedef enum { ORDER_0 = 0, ORDER_BUTT = 10 } osd_order;

typedef struct {
    uint8_t index;
    uint8_t valid;
    uint16_t width;
    uint16_t height;
    uint16_t startx;
    uint16_t starty;
    uint8_t *data;
    uint8_t bg_alpha;
    uint8_t osd_alpha;
    uint8_t video_alpha;
    osd_order add_order;
    uint32_t bg_color;
    uint32_t pixfmt;
} osd_attr;

typedef struct {
    uint8_t index;
    uint16_t width;
    uint16_t height;
    uint16_t line_width;
    uint32_t color;
    uint16_t startx;
    uint16_t starty;
} border_attr;

/* The kernel is 64-bit: its pointer is 8 bytes. */
_Static_assert(sizeof(void *) == 8, "a 64-bit target");

_Static_assert(sizeof(osd_attr) == 40, "REGION_BYTES");
_Static_assert(sizeof(osd_attr) * OSD_REGION_NUM == 320, "OSD_CONTROL_BYTES");
_Static_assert(offsetof(osd_attr, index) == 0, "index");
_Static_assert(offsetof(osd_attr, valid) == 1, "valid");
_Static_assert(offsetof(osd_attr, width) == 2, "width");
_Static_assert(offsetof(osd_attr, height) == 4, "height");
_Static_assert(offsetof(osd_attr, startx) == 6, "startx");
_Static_assert(offsetof(osd_attr, starty) == 8, "starty");
_Static_assert(offsetof(osd_attr, data) == 16, "data");
_Static_assert(offsetof(osd_attr, bg_alpha) == 24, "bg_alpha");
_Static_assert(offsetof(osd_attr, osd_alpha) == 25, "osd_alpha");
_Static_assert(offsetof(osd_attr, video_alpha) == 26, "video_alpha");
_Static_assert(offsetof(osd_attr, add_order) == 28, "add_order");
_Static_assert(sizeof(osd_order) == 4, "add_order is 32 bits");
_Static_assert(offsetof(osd_attr, bg_color) == 32, "bg_color");
_Static_assert(offsetof(osd_attr, pixfmt) == 36, "pixfmt");

_Static_assert(sizeof(border_attr) == 16, "BORDER_BYTES");
_Static_assert(offsetof(border_attr, index) == 0, "index");
_Static_assert(offsetof(border_attr, width) == 2, "width");
_Static_assert(offsetof(border_attr, height) == 4, "height");
_Static_assert(offsetof(border_attr, line_width) == 6, "line_width");
_Static_assert(offsetof(border_attr, color) == 8, "color");
_Static_assert(offsetof(border_attr, startx) == 12, "startx");
_Static_assert(offsetof(border_attr, starty) == 14, "starty");

_Static_assert(CID_NONAI2D_BASE + 0 == 0x00981B00u, "CID_MODE");
_Static_assert(CID_NONAI2D_BASE + 3 == 0x00981B03u, "CID_OSD_ATTR");
_Static_assert(CID_NONAI2D_BASE + 4 == 0x00981B04u, "CID_BORDER_ATTR");
