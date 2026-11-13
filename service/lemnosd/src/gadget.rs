//! The root phase of the daemon for USB gadgets: makes every gadget the board file describes in
//! configfs, and mounts its FunctionFS for the daemon's user, before the daemon drops to that user.
//!
//! configfs and mounts need root. [`privileged_step`] is the `usb gadget` entry of the daemon's
//! privileged start (`STEPS` in `privilege.rs`): it runs as root, before the drop, with the parsed
//! board file and the uid and gid the daemon runs as after the drop. It does what `lemnos_linux::prepare_gadget` does: the tree, the
//! mount, and the chown of each `UDC` attribute to the daemon's owner. Those are the only things
//! the unprivileged daemon writes in configfs. Its open descriptors are not needed after the drop:
//! configfs and FunctionFS check permissions when a file is opened, and the chown makes the file
//! the daemon's, so it opens `UDC` as `lemnos` after the drop.
//!
//! The settings are the ones the gadget's driver reads, through the same
//! `lemnos_driver_usb_gadget::settings`, so the tree and the daemon's descriptors agree.

use std::path::Path;

use crate::privilege::Context;
use lemnos_core::{Config, ConfigValue};
use lemnos_host::board::{BoardFile, DeviceSpec};
use lemnos_linux::{Owner, Roots, SystemMounts, prepare_gadget};

/// The root phase's step for USB gadgets (`STEPS` in `privilege.rs`): the gadgets the board file
/// describes, for the uid and gid the daemon runs as.
pub fn privileged_step(context: &Context<'_>) -> std::io::Result<()> {
    let owner = Owner {
        uid: context.uid,
        gid: context.gid,
    };
    privileged_start(context.board, owner)
        .map(|_| ())
        .map_err(std::io::Error::other)
}

/// The root phase's gadget setup for each gadget device in the board file. Returns how many
/// were set up. A device that can't be set up is named in the error, and the ones before it
/// stay set up.
pub fn privileged_start(board: &BoardFile, owner: Owner) -> Result<usize, String> {
    let mut done = 0;
    for device in board
        .devices
        .iter()
        .filter(|device| device.driver == DRIVER)
    {
        setup_one(device, owner)?;
        done += 1;
    }
    Ok(done)
}

/// The driver a gadget device names in the board file.
const DRIVER: &str = "usb-gadget";
/// Where configfs, FunctionFS mount points and the controllers are, on a Linux system.
const CONFIGFS: &str = "/sys/kernel/config";
const MOUNTS: &str = "/dev/usb-ffs";
const UDC_CLASS: &str = "/sys/class/udc";

fn setup_one(device: &DeviceSpec, owner: Owner) -> Result<(), String> {
    let id = &device.id;
    let function = device
        .uses
        .get("function")
        .ok_or_else(|| format!("{id}: no `function` need"))?;
    let name = function
        .strip_prefix("udc:")
        .ok_or_else(|| format!("{id}: `function` must be udc:NAME"))?;
    let entries = settings(device).map_err(|reason| format!("{id}: {reason}"))?;
    let config = Config::new(&entries);
    let (set, functions) = lemnos_driver_usb_gadget::settings(&config)
        .map_err(|status| format!("{id}: {status:?}"))?;
    let roots = Roots {
        configfs: Path::new(CONFIGFS),
        mounts: Path::new(MOUNTS),
        udc_class: Path::new(UDC_CLASS),
    };
    prepare_gadget(
        &roots,
        name,
        owner,
        set.bytes(),
        functions.as_slice(),
        &mut SystemMounts,
    )
    .map_err(|error| format!("{id}: {error}"))
}

/// The device's settings as the driver's [`Config`] reads them. Only the four kinds a board
/// file holds are passed; a table or an array is named as the reason.
fn settings(device: &DeviceSpec) -> Result<Vec<(&str, ConfigValue<'_>)>, String> {
    let mut entries = Vec::with_capacity(device.config.len());
    for (key, value) in &device.config {
        let value = if let Some(int) = value.as_integer() {
            ConfigValue::Int(int)
        } else if let Some(float) = value.as_float() {
            ConfigValue::Float(float)
        } else if let Some(bool) = value.as_bool() {
            ConfigValue::Bool(bool)
        } else if let Some(text) = value.as_str() {
            ConfigValue::Str(text)
        } else {
            return Err(format!(
                "setting `{key}` is not a number, boolean or string"
            ));
        };
        entries.push((key.as_str(), value));
    }
    Ok(entries)
}
