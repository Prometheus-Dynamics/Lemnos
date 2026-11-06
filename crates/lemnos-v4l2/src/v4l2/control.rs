//! Controls: query (including extended, compound and menu controls), get and set through the
//! extended control API, optionally within a media request. Shared by video nodes and
//! subdevices through the [`Controls`] trait.

use std::os::fd::{AsFd, BorrowedFd};

use lemnos_linux_sys::v4l2::device::{
    ExtControl, ExtCtrlsError, ExtCtrlsIoctl, V4L2_CTRL_MAX_DIMS, VIDIOC_G_EXT_CTRLS,
    VIDIOC_QUERY_EXT_CTRL, VIDIOC_QUERYMENU, VIDIOC_S_EXT_CTRLS, VIDIOC_TRY_EXT_CTRLS,
    v4l2_query_ext_ctrl, v4l2_querymenu, zeroed,
};

use crate::flags::flags;
use crate::node::{self, cstr_field};
use crate::{Error, Result};

const NEXT_CTRL: u32 = 0x8000_0000;
const NEXT_COMPOUND: u32 = 0x4000_0000;

const WHICH_CUR_VAL: u32 = 0;
const WHICH_DEF_VAL: u32 = 0x0f00_0000;
const WHICH_REQUEST_VAL: u32 = 0x0f01_0000;
const WHICH_MIN_VAL: u32 = 0x0f02_0000;
const WHICH_MAX_VAL: u32 = 0x0f03_0000;

/// The type of a control (`enum v4l2_ctrl_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlType {
    /// 32-bit integer.
    Integer,
    /// Boolean.
    Boolean,
    /// Menu of named items.
    Menu,
    /// Action without a value.
    Button,
    /// 64-bit integer.
    Integer64,
    /// A control class marker (not a real control).
    CtrlClass,
    /// String.
    String,
    /// 32-bit bitmask.
    Bitmask,
    /// Menu of 64-bit integers (e.g. `V4L2_CID_LINK_FREQ`).
    IntegerMenu,
    /// Array of u8.
    U8,
    /// Array of u16.
    U16,
    /// Array of u32.
    U32,
    /// `struct v4l2_area`.
    Area,
    /// `struct v4l2_rect`.
    Rect,
    /// Any other (codec, HDR, ...) compound type.
    Other(u32),
}

impl ControlType {
    /// Converts a raw type.
    pub fn from_raw(v: u32) -> Self {
        use ControlType::*;
        match v {
            1 => Integer,
            2 => Boolean,
            3 => Menu,
            4 => Button,
            5 => Integer64,
            6 => CtrlClass,
            7 => String,
            8 => Bitmask,
            9 => IntegerMenu,
            0x100 => U8,
            0x101 => U16,
            0x102 => U32,
            0x106 => Area,
            0x107 => Rect,
            other => Other(other),
        }
    }

    /// True for compound (payload) types.
    pub fn is_compound(self) -> bool {
        matches!(
            self,
            ControlType::U8
                | ControlType::U16
                | ControlType::U32
                | ControlType::Area
                | ControlType::Rect
        ) || matches!(self, ControlType::Other(v) if v >= 0x100)
    }
}

flags! {
    /// Control flags (`V4L2_CTRL_FLAG_*`).
    pub struct ControlFlags: u32 {
        const DISABLED = 0x0001;
        const GRABBED = 0x0002;
        const READ_ONLY = 0x0004;
        const UPDATE = 0x0008;
        const INACTIVE = 0x0010;
        const SLIDER = 0x0020;
        const WRITE_ONLY = 0x0040;
        const VOLATILE = 0x0080;
        const HAS_PAYLOAD = 0x0100;
        const EXECUTE_ON_WRITE = 0x0200;
        const MODIFY_LAYOUT = 0x0400;
        const DYNAMIC_ARRAY = 0x0800;
        const HAS_WHICH_MIN_MAX = 0x1000;
    }
}

/// A control's description (`VIDIOC_QUERY_EXT_CTRL`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlInfo {
    /// Control id (`V4L2_CID_*`).
    pub id: u32,
    /// Type.
    pub control_type: ControlType,
    /// Name.
    pub name: String,
    /// Minimum value.
    pub minimum: i64,
    /// Maximum value.
    pub maximum: i64,
    /// Step.
    pub step: u64,
    /// Default value.
    pub default: i64,
    /// Flags.
    pub flags: ControlFlags,
    /// Size of one element in bytes.
    pub elem_size: u32,
    /// Number of elements (product of `dims`).
    pub elems: u32,
    /// Array dimensions (empty for scalars).
    pub dims: Vec<u32>,
}

impl ControlInfo {
    /// True when the value is passed by pointer (strings, arrays, compound types).
    pub fn has_payload(&self) -> bool {
        self.flags.contains(ControlFlags::HAS_PAYLOAD)
    }

    /// True for menu and integer-menu controls.
    pub fn is_menu(&self) -> bool {
        matches!(
            self.control_type,
            ControlType::Menu | ControlType::IntegerMenu
        )
    }

    /// True when the value can be read (not write-only, a button or a class marker).
    pub fn is_readable(&self) -> bool {
        !self.flags.contains(ControlFlags::WRITE_ONLY)
            && !matches!(
                self.control_type,
                ControlType::Button | ControlType::CtrlClass
            )
    }

    fn from_raw(raw: &v4l2_query_ext_ctrl) -> Self {
        let nr = (raw.nr_of_dims as usize).min(V4L2_CTRL_MAX_DIMS);
        Self {
            id: raw.id,
            control_type: ControlType::from_raw(raw.type_),
            name: cstr_field(&raw.name),
            minimum: raw.minimum,
            maximum: raw.maximum,
            step: raw.step,
            default: raw.default_value,
            flags: ControlFlags(raw.flags),
            elem_size: raw.elem_size,
            elems: raw.elems,
            dims: raw.dims[..nr].to_vec(),
        }
    }
}

/// The value of a menu item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuValue {
    /// A named item (`Menu`).
    Name(String),
    /// An integer item (`IntegerMenu`).
    Integer(i64),
}

/// One item of a menu control (`VIDIOC_QUERYMENU`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Item index (the control value that selects it).
    pub index: u32,
    /// Item value.
    pub value: MenuValue,
}

/// A control value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlValue {
    /// Integer, boolean (0/1), menu index, bitmask or button (any value) controls.
    Integer(i32),
    /// 64-bit integer controls.
    Integer64(i64),
    /// String controls.
    String(String),
    /// Compound or array controls: the raw payload.
    Payload(Vec<u8>),
}

impl ControlValue {
    /// The value as an integer, for `Integer` and `Integer64`.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ControlValue::Integer(v) => Some(i64::from(*v)),
            ControlValue::Integer64(v) => Some(*v),
            _ => None,
        }
    }
}

/// Which value set a control operation reads or writes.
#[derive(Clone, Copy, Debug)]
pub enum ControlWhich<'fd> {
    /// The current values.
    Current,
    /// The default values (read-only).
    Default,
    /// The values stored in (or to be applied with) a media request.
    Request(BorrowedFd<'fd>),
    /// The minimum values (read-only; controls with `HAS_WHICH_MIN_MAX`).
    Minimum,
    /// The maximum values (read-only; controls with `HAS_WHICH_MIN_MAX`).
    Maximum,
}

impl<'fd> ControlWhich<'fd> {
    /// The `which` field and the request descriptor the kernel takes for this choice.
    fn raw(self) -> (u32, Option<BorrowedFd<'fd>>) {
        match self {
            ControlWhich::Current => (WHICH_CUR_VAL, None),
            ControlWhich::Default => (WHICH_DEF_VAL, None),
            ControlWhich::Request(fd) => (WHICH_REQUEST_VAL, Some(fd)),
            ControlWhich::Minimum => (WHICH_MIN_VAL, None),
            ControlWhich::Maximum => (WHICH_MAX_VAL, None),
        }
    }
}

/// The storage behind one control of a call: its id, which union member it uses, the value
/// (32- and 64-bit controls), and the payload (strings and compound controls) the kernel
/// reads or fills through a pointer.
struct Slot {
    id: u32,
    kind: SlotKind,
    value: i64,
    /// The payload's size; the kernel updates it for dynamic arrays.
    size: u32,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Value,
    Value64,
    String,
    Payload,
}

impl Slot {
    fn new(id: u32, kind: SlotKind, value: i64, payload: Vec<u8>) -> Self {
        Self {
            id,
            kind,
            value,
            size: payload.len() as u32,
            payload,
        }
    }

    fn for_get(info: &ControlInfo) -> Self {
        let kind = match info.control_type {
            _ if info.has_payload() && info.control_type == ControlType::String => SlotKind::String,
            _ if info.has_payload() => SlotKind::Payload,
            ControlType::Integer64 => SlotKind::Value64,
            _ => SlotKind::Value,
        };
        let payload = match kind {
            // A string control's maximum is its maximum length, excluding the NUL.
            SlotKind::String => {
                vec![0u8; (info.maximum.max(0) as usize + 1) * info.elems.max(1) as usize]
            }
            SlotKind::Payload => vec![0u8; info.elem_size as usize * info.elems.max(1) as usize],
            _ => Vec::new(),
        };
        Self::new(info.id, kind, 0, payload)
    }

    fn for_set(id: u32, value: &ControlValue) -> Self {
        match value {
            ControlValue::Integer(v) => Self::new(id, SlotKind::Value, i64::from(*v), Vec::new()),
            ControlValue::Integer64(v) => Self::new(id, SlotKind::Value64, *v, Vec::new()),
            ControlValue::String(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                Self::new(id, SlotKind::String, 0, bytes)
            }
            ControlValue::Payload(p) => Self::new(id, SlotKind::Payload, 0, p.clone()),
        }
    }

    /// The `v4l2_ext_control` for this slot, borrowing its payload for the call.
    fn control(&mut self) -> ExtControl<'_> {
        match self.kind {
            // Truncation is the point: a 32-bit control carries `i32`.
            SlotKind::Value => ExtControl::value(self.id, self.value as i32),
            SlotKind::Value64 => ExtControl::value64(self.id, self.value),
            SlotKind::String | SlotKind::Payload => ExtControl::payload(self.id, &mut self.payload),
        }
    }

    fn value(self) -> ControlValue {
        match self.kind {
            SlotKind::Value => ControlValue::Integer(self.value as i32),
            SlotKind::Value64 => ControlValue::Integer64(self.value),
            SlotKind::String => ControlValue::String(cstr_field(&self.payload)),
            SlotKind::Payload => {
                let mut p = self.payload;
                // Dynamic arrays report the used size back in `size`.
                p.truncate(self.size as usize);
                ControlValue::Payload(p)
            }
        }
    }
}

/// The most controls a set or try call holds on the stack (no allocation): a sensor's
/// exposure, gain and frame length, with room to spare. More go through [`Slot`]s.
const INLINE_CONTROLS: usize = 8;

/// Turns a refused call into an error naming the control the kernel blames, when it blames
/// one (`id_at` maps the kernel's index to a control id).
fn ext_error(
    name: &'static str,
    error: ExtCtrlsError,
    id_at: impl FnOnce(usize) -> Option<u32>,
) -> Error {
    let ExtCtrlsError { source, error_idx } = error;
    match (source.raw_os_error(), id_at(error_idx as usize)) {
        (Some(errno), Some(control)) => Error::Control {
            name,
            errno,
            control,
        },
        (Some(errno), None) => Error::Ioctl { name, errno },
        (None, _) => Error::Sys { call: name, source },
    }
}

/// Runs one extended-controls call over `slots`, copying what the kernel returned back into
/// them.
fn run_slots(
    fd: BorrowedFd<'_>,
    request: ExtCtrlsIoctl,
    which: ControlWhich<'_>,
    slots: &mut [Slot],
) -> Result<()> {
    let (which, request_fd) = which.raw();
    let meta: Vec<(u32, SlotKind)> = slots.iter().map(|s| (s.id, s.kind)).collect();
    // The payload borrows end with `ctrls`; only then can the slots be written again.
    let (outcome, back) = {
        let mut ctrls: Vec<ExtControl<'_>> = slots.iter_mut().map(Slot::control).collect();
        let outcome = request.call(fd, which, request_fd, &mut ctrls);
        let back: Vec<(u32, i64)> = ctrls
            .iter()
            .zip(&meta)
            .map(|(ctrl, &(_, kind))| match kind {
                SlotKind::Value => (ctrl.size(), i64::from(ctrl.get_value())),
                SlotKind::Value64 => (ctrl.size(), ctrl.get_value64()),
                SlotKind::String | SlotKind::Payload => (ctrl.size(), 0),
            })
            .collect();
        (outcome, back)
    };
    for (slot, (size, value)) in slots.iter_mut().zip(back) {
        slot.size = size;
        slot.value = value;
    }
    outcome.map_err(|e| ext_error(request.name(), e, |i| meta.get(i).map(|m| m.0)))
}

/// Integer values (at most [`INLINE_CONTROLS`]) as controls on the stack, or `None` when the
/// values need payloads or there are more of them.
fn inline_integers(
    values: &[(u32, ControlValue)],
) -> Option<[ExtControl<'static>; INLINE_CONTROLS]> {
    if values.len() > INLINE_CONTROLS {
        return None;
    }
    let mut ctrls: [ExtControl<'static>; INLINE_CONTROLS] =
        core::array::from_fn(|_| ExtControl::value(0, 0));
    for (ctrl, (id, value)) in ctrls.iter_mut().zip(values) {
        *ctrl = match value {
            ControlValue::Integer(x) => ExtControl::value(*id, *x),
            ControlValue::Integer64(x) => ExtControl::value64(*id, *x),
            _ => return None,
        };
    }
    Some(ctrls)
}

/// A set or try of `values` (`S_EXT_CTRLS` or `TRY_EXT_CTRLS`), on the stack when it can be.
fn write_controls(
    fd: BorrowedFd<'_>,
    request: ExtCtrlsIoctl,
    which: ControlWhich<'_>,
    values: &[(u32, ControlValue)],
) -> Result<()> {
    if let Some(mut ctrls) = inline_integers(values) {
        let (which, request_fd) = which.raw();
        return request
            .call(fd, which, request_fd, &mut ctrls[..values.len()])
            .map_err(|e| ext_error(request.name(), e, |i| values.get(i).map(|v| v.0)));
    }
    let mut slots: Vec<Slot> = values.iter().map(|(id, v)| Slot::for_set(*id, v)).collect();
    run_slots(fd, request, which, &mut slots)
}

/// The control API, shared by video nodes and subdevices.
pub trait Controls: AsFd {
    /// Describes one control (`VIDIOC_QUERY_EXT_CTRL`).
    fn query_control(&self, id: u32) -> Result<ControlInfo> {
        let mut raw: v4l2_query_ext_ctrl = zeroed();
        raw.id = id;
        node::ioctl(self.as_fd(), VIDIOC_QUERY_EXT_CTRL, &mut raw)?;
        Ok(ControlInfo::from_raw(&raw))
    }

    /// Describes every control, including compound controls and class markers
    /// (`VIDIOC_QUERY_EXT_CTRL` with `NEXT_CTRL | NEXT_COMPOUND`).
    fn query_controls(&self) -> Result<Vec<ControlInfo>> {
        let mut out = Vec::new();
        let mut id = NEXT_CTRL | NEXT_COMPOUND;
        loop {
            let mut raw: v4l2_query_ext_ctrl = zeroed();
            raw.id = id;
            match node::ioctl(self.as_fd(), VIDIOC_QUERY_EXT_CTRL, &mut raw) {
                Ok(()) => {}
                Err(e) if e.is_invalid_argument() => break,
                Err(e) if e.is_not_supported() && out.is_empty() => break,
                Err(e) => return Err(e),
            }
            let info = ControlInfo::from_raw(&raw);
            id = info.id | NEXT_CTRL | NEXT_COMPOUND;
            out.push(info);
        }
        Ok(out)
    }

    /// Lists the items of a menu or integer-menu control (`VIDIOC_QUERYMENU`); indexes the
    /// driver skips are left out.
    fn query_menu(&self, info: &ControlInfo) -> Result<Vec<MenuItem>> {
        if !info.is_menu() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for index in info.minimum.max(0)..=info.maximum.max(-1) {
            let mut raw: v4l2_querymenu = zeroed();
            raw.id = info.id;
            raw.index = index as u32;
            match node::ioctl(self.as_fd(), VIDIOC_QUERYMENU, &mut raw) {
                Ok(()) => {}
                Err(e) if e.is_invalid_argument() => continue,
                Err(e) => return Err(e),
            }
            let name = raw.name;
            let value = if info.control_type == ControlType::IntegerMenu {
                let mut v = [0u8; 8];
                v.copy_from_slice(&name[..8]);
                MenuValue::Integer(i64::from_ne_bytes(v))
            } else {
                MenuValue::Name(cstr_field(&name))
            };
            out.push(MenuItem {
                index: index as u32,
                value,
            });
        }
        Ok(out)
    }

    /// Reads one control's current value.
    fn control(&self, id: u32) -> Result<ControlValue> {
        let info = self.query_control(id)?;
        let mut values =
            self.get_controls_for(ControlWhich::Current, std::slice::from_ref(&info))?;
        Ok(values.remove(0))
    }

    /// Sets one control's current value.
    fn set_control(&self, id: u32, value: ControlValue) -> Result<()> {
        self.set_controls(ControlWhich::Current, &[(id, value)])
    }

    /// Reads several controls atomically (`VIDIOC_G_EXT_CTRLS`), querying each one's type first.
    fn get_controls(&self, which: ControlWhich<'_>, ids: &[u32]) -> Result<Vec<ControlValue>> {
        let infos = ids
            .iter()
            .map(|&id| self.query_control(id))
            .collect::<Result<Vec<_>>>()?;
        self.get_controls_for(which, &infos)
    }

    /// Reads several controls atomically (`VIDIOC_G_EXT_CTRLS`) whose descriptions are known.
    fn get_controls_for(
        &self,
        which: ControlWhich<'_>,
        infos: &[ControlInfo],
    ) -> Result<Vec<ControlValue>> {
        let mut slots: Vec<Slot> = infos.iter().map(Slot::for_get).collect();
        run_slots(self.as_fd(), VIDIOC_G_EXT_CTRLS, which, &mut slots)?;
        Ok(slots.into_iter().map(Slot::value).collect())
    }

    /// Sets several controls atomically (`VIDIOC_S_EXT_CTRLS`). With
    /// [`ControlWhich::Request`], the values are stored in the request and applied when it is
    /// queued.
    fn set_controls(&self, which: ControlWhich<'_>, values: &[(u32, ControlValue)]) -> Result<()> {
        write_controls(self.as_fd(), VIDIOC_S_EXT_CTRLS, which, values)
    }

    /// Checks whether values would be accepted, without applying them
    /// (`VIDIOC_TRY_EXT_CTRLS`).
    fn try_controls(&self, which: ControlWhich<'_>, values: &[(u32, ControlValue)]) -> Result<()> {
        write_controls(self.as_fd(), VIDIOC_TRY_EXT_CTRLS, which, values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        control_type: ControlType,
        flags: ControlFlags,
        maximum: i64,
        elem_size: u32,
        elems: u32,
    ) -> ControlInfo {
        ControlInfo {
            id: 0x0098_0900,
            control_type,
            name: "test".into(),
            minimum: 0,
            maximum,
            step: 1,
            default: 0,
            flags,
            elem_size,
            elems,
            dims: Vec::new(),
        }
    }

    #[test]
    fn slots_pick_the_right_union_member() {
        let s = Slot::for_get(&info(ControlType::Integer, ControlFlags::empty(), 10, 4, 1));
        assert!(s.kind == SlotKind::Value && s.payload.is_empty());
        let s = Slot::for_get(&info(
            ControlType::Integer64,
            ControlFlags::empty(),
            10,
            8,
            1,
        ));
        assert!(s.kind == SlotKind::Value64);
        let s = Slot::for_get(&info(
            ControlType::String,
            ControlFlags::HAS_PAYLOAD,
            31,
            32,
            1,
        ));
        assert!(s.kind == SlotKind::String && s.payload.len() == 32);
        let mut s = Slot::for_get(&info(ControlType::U16, ControlFlags::HAS_PAYLOAD, 0, 2, 12));
        assert!(s.kind == SlotKind::Payload && s.payload.len() == 24);
        assert_eq!(s.control().size(), 24);
    }

    #[test]
    fn set_slots_carry_values() {
        let s = Slot::for_set(1, &ControlValue::Integer(-5));
        assert_eq!(s.value(), ControlValue::Integer(-5));
        let s = Slot::for_set(1, &ControlValue::Integer64(1 << 40));
        assert_eq!(s.value(), ControlValue::Integer64(1 << 40));
        let s = Slot::for_set(1, &ControlValue::String("abc".into()));
        assert_eq!(s.size, 4);
        assert_eq!(s.value(), ControlValue::String("abc".into()));
    }

    #[test]
    fn inline_integers_only_take_small_integer_sets() {
        let few = [
            (1u32, ControlValue::Integer(3)),
            (2, ControlValue::Integer64(9)),
        ];
        assert!(inline_integers(&few).is_some());
        let text = [(1u32, ControlValue::String("x".into()))];
        assert!(inline_integers(&text).is_none());
        let many: Vec<(u32, ControlValue)> =
            (0..9).map(|i| (i, ControlValue::Integer(0))).collect();
        assert!(inline_integers(&many).is_none());
    }

    #[test]
    fn control_types_decode() {
        assert_eq!(ControlType::from_raw(9), ControlType::IntegerMenu);
        assert!(ControlType::from_raw(0x107).is_compound());
        assert!(ControlType::from_raw(0x200).is_compound());
        assert!(!ControlType::Integer.is_compound());
    }

    #[test]
    fn which_selects_the_kernel_field() {
        assert_eq!(ControlWhich::Current.raw().0, 0);
        assert_eq!(ControlWhich::Default.raw().0, WHICH_DEF_VAL);
        assert_eq!(ControlWhich::Minimum.raw().0, WHICH_MIN_VAL);
        assert_eq!(ControlWhich::Maximum.raw().0, WHICH_MAX_VAL);
        assert!(ControlWhich::Current.raw().1.is_none());
    }
}
