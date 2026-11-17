// SPDX-License-Identifier: GPL-2.0
//! lemnos_rs: the Rust half of lemnos.ko (stage 0, the skeleton).
//!
//! A platform driver and a miscdevice. The driver has no match table of its own: it binds only when
//! `driver_override` names a device, as the C module does, and the device's compatible is listed.
//! The miscdevice is registered when a device binds. Names are `lemnos-rs` until the port replaces
//! the C module, so both can load in one guest.

use kernel::{
    alloc::KBox,
    device::Core,
    fs::File,
    macros::vtable,
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    of,
    platform,
    prelude::*,
    sync::aref::ARef,
};

/// The miscdevice's state, one per open file. The skeleton has nothing to hold yet.
struct LemnosRsFile;

struct LemnosRsMisc;

#[vtable]
impl MiscDevice for LemnosRsMisc {
    type Ptr = Pin<KBox<LemnosRsFile>>;

    fn open(_file: &File, _misc: &MiscDeviceRegistration<Self>) -> Result<Pin<KBox<LemnosRsFile>>> {
        Ok(KBox::into_pin(KBox::new(LemnosRsFile, GFP_KERNEL)?))
    }

    fn ioctl(_me: Pin<&LemnosRsFile>, _file: &File, _cmd: u32, _arg: usize) -> Result<isize> {
        Err(ENOTTY)
    }
}

/// The bound device. Holding the registration keeps the node until unbind.
struct LemnosRsPlat {
    _pdev: ARef<platform::Device>,
    _misc: Pin<KBox<MiscDeviceRegistration<LemnosRsMisc>>>,
}

kernel::of_device_table!(
    OF_TABLE,
    <LemnosRsPlat as platform::Driver>::IdInfo,
    [(of::DeviceId::new(c"lemnos,rs-skeleton"), ())]
);

impl platform::Driver for LemnosRsPlat {
    type IdInfo = ();
    type Data<'bound> = Self;
    const OF_ID_TABLE: Option<of::IdTable<Self::IdInfo>> = Some(&OF_TABLE);

    fn probe<'bound>(
        pdev: &'bound platform::Device<Core<'_>>,
        _info: Option<&'bound Self::IdInfo>,
    ) -> impl PinInit<Self, Error> + 'bound {
        let opts = MiscDeviceOptions {
            name: c"lemnos-rs",
        };
        Ok(Self {
            _pdev: pdev.into(),
            _misc: KBox::pin_init(MiscDeviceRegistration::register(opts), GFP_KERNEL)?,
        })
    }
}

kernel::module_platform_driver! {
    type: LemnosRsPlat,
    name: "lemnos_rs",
    authors: ["Lemnos"],
    description: "Lemnos kernel module, Rust port: stage 0 skeleton",
    license: "GPL",
}
