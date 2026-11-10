//! A job's parameters and results as the vendor driver takes them: `struct iep2_params` and
//! `struct iep2_output` of `drivers/video/rockchip/mpp/mpp_iep2.c` (rockchip-linux/kernel
//! `develop-6.1` at commit `77168c8d5ab82399f65a80e9f807b50ba37cf483`, `GPL-2.0+ OR MIT`), with
//! the sizes and offsets of the C compiler on LP64 pinned by the tests. The structures are
//! private to that driver and reach the kernel as message data, so they are this crate's to
//! carry, not a uAPI header's.

/// Address words in `src` and `dst`.
const PLANES: usize = 3;
/// OSD areas, motion truth list entries and combing OSD flags.
pub const OSD_AREAS: usize = 8;
/// Entries of the motion table.
pub const MTN_TAB_LEN: usize = 16;
/// Bins of the motion-vector histogram (`MVL + MVR + 1`).
pub const MV_HIST_BINS: usize = 28 + 27 + 1;

/// `struct iep2_addr`: one image's planes, each a descriptor until the kernel turns it into an
/// address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Addr {
    /// Luma.
    pub y: u32,
    /// Chroma, or Cb of a planar image.
    pub cbcr: u32,
    /// Cr of a planar image.
    pub cr: u32,
}

/// `struct iep2_params`. Only a [`Job`](crate::Job) fills one in for the kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Params {
    /// Input sampling, a [`Sampling`].
    pub src_fmt: u32,
    /// Input chroma layout, a [`Swap`].
    pub src_yuv_swap: u32,
    /// Output sampling.
    pub dst_fmt: u32,
    /// Output chroma layout.
    pub dst_yuv_swap: u32,
    /// Width in 16-pixel tiles.
    pub tile_cols: u32,
    /// Height in 4-line tiles.
    pub tile_rows: u32,
    /// Input luma stride, in 4-byte words.
    pub src_y_stride: u32,
    /// Input chroma stride, in 4-byte words.
    pub src_uv_stride: u32,
    /// Output stride, in 4-byte words.
    pub dst_y_stride: u32,
    /// The current, next and previous input frames.
    pub src: [Addr; PLANES],
    /// The top and bottom output frames.
    pub dst: [Addr; 2],
    /// Motion vectors, read and written.
    pub mv_addr: u32,
    /// Motion data, read and written.
    pub md_addr: u32,
    /// A [`DilMode`].
    pub dil_mode: u32,
    /// Always zero here.
    pub dil_out_mode: u32,
    /// A [`FieldOrder`].
    pub dil_field_order: u32,
    pub md_theta: u32,
    pub md_r: u32,
    pub md_lambda: u32,
    pub dect_resi_thr: u32,
    /// OSD areas programmed, at most [`OSD_AREAS`].
    pub osd_area_num: u32,
    pub osd_gradh_thr: u32,
    pub osd_gradv_thr: u32,
    pub osd_pos_limit_en: u32,
    pub osd_pos_limit_num: u32,
    pub osd_limit_area: [u32; 2],
    pub osd_line_num: u32,
    pub osd_pec_thr: u32,
    pub osd_x_sta: [u32; OSD_AREAS],
    pub osd_x_end: [u32; OSD_AREAS],
    pub osd_y_sta: [u32; OSD_AREAS],
    pub osd_y_end: [u32; OSD_AREAS],
    pub me_pena: u32,
    pub mv_bonus: u32,
    pub mv_similar_thr: u32,
    pub mv_similar_num_thr0: u32,
    pub me_thr_offset: i32,
    pub mv_left_limit: u32,
    pub mv_right_limit: u32,
    pub mv_tru_list: [i8; OSD_AREAS],
    pub mv_tru_vld: [u32; OSD_AREAS],
    pub eedi_thr0: u32,
    pub ble_backtoma_num: u32,
    pub comb_cnt_thr: u32,
    pub comb_feature_thr: u32,
    pub comb_t_thr: u32,
    pub comb_osd_vld: [u32; OSD_AREAS],
    /// Nonzero uses `mtn_tab`, zero the kernel's own table.
    pub mtn_en: u32,
    pub mtn_tab: [u32; MTN_TAB_LEN],
    /// A [`PdComp`], read in [`DilMode::Pd`].
    pub pd_mode: u32,
    /// Always zero here: the kernel's ROI buffer is never filled.
    pub roi_en: u32,
    /// Unused by the kernel; zero.
    pub roi_layer_num: u32,
    /// Unused by the kernel; zero.
    pub roi_mode: [u32; OSD_AREAS],
    /// Unused by the kernel; zero.
    pub xsta: [u32; OSD_AREAS],
    /// Unused by the kernel; zero.
    pub xend: [u32; OSD_AREAS],
    /// Unused by the kernel; zero.
    pub ysta: [u32; OSD_AREAS],
    /// Unused by the kernel; zero.
    pub yend: [u32; OSD_AREAS],
}

impl Params {
    /// The address words in the kernel's order (see
    /// [`ADDRESS_REGISTERS`](crate::regs::ADDRESS_REGISTERS)).
    #[must_use]
    pub fn address_words(&self) -> [u32; crate::regs::ADDRESS_WORDS] {
        let [cur, next, prev] = self.src;
        let [top, bottom] = self.dst;
        [
            cur.y,
            cur.cbcr,
            cur.cr,
            next.y,
            next.cbcr,
            next.cr,
            prev.y,
            prev.cbcr,
            prev.cr,
            top.y,
            top.cbcr,
            top.cr,
            bottom.y,
            bottom.cbcr,
            bottom.cr,
            self.mv_addr,
            self.md_addr,
        ]
    }
}

/// `struct iep2_output`: what the block counted, copied out when the job is done.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Results {
    pub mv_hist: [u32; MV_HIST_BINS],
    pub dect_pd_tcnt: u32,
    pub dect_pd_bcnt: u32,
    pub dect_ff_cur_tcnt: u32,
    pub dect_ff_cur_bcnt: u32,
    pub dect_ff_nxt_tcnt: u32,
    pub dect_ff_nxt_bcnt: u32,
    pub dect_ff_ble_tcnt: u32,
    pub dect_ff_ble_bcnt: u32,
    pub dect_ff_nz: u32,
    pub dect_ff_comb_f: u32,
    pub dect_osd_cnt: u32,
    pub out_comb_cnt: u32,
    pub out_osd_comb_cnt: u32,
    pub ff_gradt_tcnt: u32,
    pub ff_gradt_bcnt: u32,
    pub x_sta: [u32; OSD_AREAS],
    pub x_end: [u32; OSD_AREAS],
    pub y_sta: [u32; OSD_AREAS],
    pub y_end: [u32; OSD_AREAS],
}

impl Default for Results {
    fn default() -> Self {
        Self {
            mv_hist: [0; MV_HIST_BINS],
            dect_pd_tcnt: 0,
            dect_pd_bcnt: 0,
            dect_ff_cur_tcnt: 0,
            dect_ff_cur_bcnt: 0,
            dect_ff_nxt_tcnt: 0,
            dect_ff_nxt_bcnt: 0,
            dect_ff_ble_tcnt: 0,
            dect_ff_ble_bcnt: 0,
            dect_ff_nz: 0,
            dect_ff_comb_f: 0,
            dect_osd_cnt: 0,
            out_comb_cnt: 0,
            out_osd_comb_cnt: 0,
            ff_gradt_tcnt: 0,
            ff_gradt_bcnt: 0,
            x_sta: [0; OSD_AREAS],
            x_end: [0; OSD_AREAS],
            y_sta: [0; OSD_AREAS],
            y_end: [0; OSD_AREAS],
        }
    }
}

/// Chroma sampling (`enum rockchip_iep2_fmt`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Sampling {
    /// 4:2:2: chroma on every line.
    Yuv422 = 2,
    /// 4:2:0: chroma on every other line.
    Yuv420 = 3,
}

impl Sampling {
    /// Chroma lines of an image `height` lines high.
    #[must_use]
    pub const fn chroma_lines(self, height: u32) -> u32 {
        match self {
            Self::Yuv422 => height,
            Self::Yuv420 => height / 2,
        }
    }
}

/// How chroma is laid out (`enum rockchip_iep2_yuv_swap`). `ROCKCHIP_IEP2_YUV_SWAP_P0` (2),
/// whose layout the source does not describe, is left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Swap {
    /// One interleaved chroma plane, Cb first.
    SemiUv = 0,
    /// One interleaved chroma plane, Cr first.
    SemiVu = 1,
    /// Separate Cb and Cr planes. Input only: the output has no third address.
    Planar = 3,
}

/// The deinterlacing mode (`enum rockchip_iep2_dil_mode`). `ROCKCHIP_IEP2_DIL_MODE_I1O1T` (5) is
/// left out: the vendor driver notes that in it the block reads past the end of the current
/// frame's buffer, and maps a spare page there when the read faults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DilMode {
    Disable = 0,
    I5O2 = 1,
    I5O1T = 2,
    I5O1B = 3,
    I2O2 = 4,
    I1O1B = 6,
    /// Pulldown: the fields come from the frames the job's [`PdComp`] names.
    Pd = 7,
    Bypass = 8,
    Dect = 9,
}

/// Field order (`enum rockchip_iep2_dil_ff_order`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum FieldOrder {
    /// Top field first.
    TopFirst = 0,
    /// Bottom field first.
    BottomFirst = 1,
}

/// Which frames make the fields in pulldown mode (`enum ROCKCHIP_IEP2_PD_COMP_FLAG`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PdComp {
    /// Both from the current frame.
    CurCur = 0,
    /// Top from the current, bottom from the next.
    CurNext = 1,
    /// Top from the next, bottom from the current.
    NextCur = 2,
}

/// The caller's settings for the block's detectors and filters. Lemnos carries them as they are
/// and never reads them, except that `osd_area_num` must be at most [`OSD_AREAS`]: the vendor
/// driver indexes its arrays by it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tuning {
    pub md_theta: u32,
    pub md_r: u32,
    pub md_lambda: u32,
    pub dect_resi_thr: u32,
    pub osd_area_num: u32,
    pub osd_gradh_thr: u32,
    pub osd_gradv_thr: u32,
    pub osd_pos_limit_en: u32,
    pub osd_pos_limit_num: u32,
    pub osd_limit_area: [u32; 2],
    pub osd_line_num: u32,
    pub osd_pec_thr: u32,
    pub osd_x_sta: [u32; OSD_AREAS],
    pub osd_x_end: [u32; OSD_AREAS],
    pub osd_y_sta: [u32; OSD_AREAS],
    pub osd_y_end: [u32; OSD_AREAS],
    pub me_pena: u32,
    pub mv_bonus: u32,
    pub mv_similar_thr: u32,
    pub mv_similar_num_thr0: u32,
    pub me_thr_offset: i32,
    pub mv_left_limit: u32,
    pub mv_right_limit: u32,
    pub mv_tru_list: [i8; OSD_AREAS],
    pub mv_tru_vld: [u32; OSD_AREAS],
    pub eedi_thr0: u32,
    pub ble_backtoma_num: u32,
    pub comb_cnt_thr: u32,
    pub comb_feature_thr: u32,
    pub comb_t_thr: u32,
    pub comb_osd_vld: [u32; OSD_AREAS],
    /// The motion table, or `None` for the kernel's own.
    pub mtn_tab: Option<[u32; MTN_TAB_LEN]>,
}

impl Tuning {
    /// Copies the settings into `params`.
    pub(crate) fn apply(&self, params: &mut Params) {
        params.md_theta = self.md_theta;
        params.md_r = self.md_r;
        params.md_lambda = self.md_lambda;
        params.dect_resi_thr = self.dect_resi_thr;
        params.osd_area_num = self.osd_area_num;
        params.osd_gradh_thr = self.osd_gradh_thr;
        params.osd_gradv_thr = self.osd_gradv_thr;
        params.osd_pos_limit_en = self.osd_pos_limit_en;
        params.osd_pos_limit_num = self.osd_pos_limit_num;
        params.osd_limit_area = self.osd_limit_area;
        params.osd_line_num = self.osd_line_num;
        params.osd_pec_thr = self.osd_pec_thr;
        params.osd_x_sta = self.osd_x_sta;
        params.osd_x_end = self.osd_x_end;
        params.osd_y_sta = self.osd_y_sta;
        params.osd_y_end = self.osd_y_end;
        params.me_pena = self.me_pena;
        params.mv_bonus = self.mv_bonus;
        params.mv_similar_thr = self.mv_similar_thr;
        params.mv_similar_num_thr0 = self.mv_similar_num_thr0;
        params.me_thr_offset = self.me_thr_offset;
        params.mv_left_limit = self.mv_left_limit;
        params.mv_right_limit = self.mv_right_limit;
        params.mv_tru_list = self.mv_tru_list;
        params.mv_tru_vld = self.mv_tru_vld;
        params.eedi_thr0 = self.eedi_thr0;
        params.ble_backtoma_num = self.ble_backtoma_num;
        params.comb_cnt_thr = self.comb_cnt_thr;
        params.comb_feature_thr = self.comb_feature_thr;
        params.comb_t_thr = self.comb_t_thr;
        params.comb_osd_vld = self.comb_osd_vld;
        match self.mtn_tab {
            Some(table) => {
                params.mtn_en = 1;
                params.mtn_tab = table;
            }
            None => {
                params.mtn_en = 0;
                params.mtn_tab = [0; MTN_TAB_LEN];
            }
        }
    }
}
