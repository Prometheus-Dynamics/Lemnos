//! Board definitions in the runtime (feature `board`).
//!
//! A [`BoardSetup`] turns a [`BoardDefinition`] (`lemnos-board`) into what
//! the runtime needs:
//!
//! - a [`BoardProbe`] that reports every device of the board as a
//!   configured descriptor (`board.<board>.<device>`), on I2C or as a
//!   [`InterfaceKind::Platform`] device;
//! - two generic drivers (`lemnos.board.i2c`, `lemnos.board.platform`) that
//!   build each device with the board's [`DriverRegistry`] and bind it
//!   through the device-model adapter
//!   ([`lemnos_driver_sdk::l1::L1BoundDevice`]): channels become telemetry
//!   (`acceleration.x`, `bus_voltage`, ...), controls become `<control>.set`
//!   interactions. I2C devices get their bus from the runtime's I2C backend,
//!   so the same definition binds against Linux or mock hardware.
//!
//! With the `linux` feature:
//!
#![cfg_attr(feature = "linux", doc = "```no_run")]
#![cfg_attr(not(feature = "linux"), doc = "```ignore")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use lemnos::board::{BoardDefinition, BoardSetup};
//! use lemnos::prelude::*;
//! use std::sync::Arc;
//!
//! let setup = BoardSetup::new(BoardDefinition::from_path("/etc/helios/sensors.toml")?)?;
//! let backend = LinuxBackend::default().with_probe(Arc::new(setup.probe()));
//! let mut lemnos = Lemnos::builder()
//!     .with_linux_backend_ref(&backend)
//!     .with_board(&setup)?
//!     .build();
//! lemnos.refresh_with_linux(&DiscoveryContext::new(), &backend)?;
//! # Ok(())
//! # }
//! ```

use lemnos_board::DriverEntry;
pub use lemnos_board::{
    Backend, BoardDefinition, BoardError, BoardInfo, BusRef, Buses, ConfigValue, DeviceSpec,
    DriverRegistry, DynI2c, I2cSelector, Interface,
};
use lemnos_bus::hal::OwnedI2cBus;
use lemnos_core::{DeviceAddress, DeviceDescriptor, DeviceId, DeviceKind, InterfaceKind, Value};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use lemnos_driver_sdk::l1::{L1Driver, PROPERTY_CLASS, PROPERTY_DRIVER};
use lemnos_driver_sdk::{DriverBindContext, DriverError, SessionAccess};
pub use lemnos_drivers_linux::SysRoot;
use lemnos_hal::HalError;
use std::sync::Arc;

/// The driver that binds a board's I2C devices.
pub const I2C_DRIVER: &str = "lemnos.board.i2c";
/// The driver that binds a board's platform devices (fans, thermal zones,
/// kernel class devices).
pub const PLATFORM_DRIVER: &str = "lemnos.board.platform";
/// Descriptor property: the board id.
pub const PROPERTY_BOARD: &str = "board.id";
/// Descriptor property: the device id within the board.
pub const PROPERTY_DEVICE: &str = "board.device";
/// Descriptor property: the board driver name (`bmi088`).
pub const PROPERTY_BOARD_DRIVER: &str = "board.driver";

const INTERFACES: [InterfaceKind; 2] = [InterfaceKind::I2c, InterfaceKind::Platform];

/// A validated board definition, ready to add to a runtime.
#[derive(Debug, Clone)]
pub struct BoardSetup {
    board: Arc<BoardDefinition>,
    registry: Arc<DriverRegistry>,
    sys: SysRoot,
}

impl BoardSetup {
    /// Validates `board` against the built-in drivers.
    pub fn new(board: BoardDefinition) -> Result<Self, BoardError> {
        Self::with_registry(board, DriverRegistry::builtin())
    }

    /// Validates `board` against `registry`.
    pub fn with_registry(
        board: BoardDefinition,
        registry: DriverRegistry,
    ) -> Result<Self, BoardError> {
        board.validate(&registry)?;
        Ok(Self {
            board: Arc::new(board),
            registry: Arc::new(registry),
            sys: SysRoot::default(),
        })
    }

    /// Looks for kernel class devices (fans, thermal zones, IIO and hwmon
    /// drivers) under `sys` instead of `/sys`.
    pub fn with_sys_root(mut self, sys: impl Into<std::path::PathBuf>) -> Self {
        self.sys = SysRoot::new(sys);
        self
    }

    pub fn board(&self) -> &BoardDefinition {
        &self.board
    }

    /// The probe that reports the board's devices; pass it to refreshes, or
    /// add it to a `LinuxBackend` with `with_probe`.
    pub fn probe(&self) -> BoardProbe {
        BoardProbe {
            board: Arc::clone(&self.board),
            registry: Arc::clone(&self.registry),
            sys: self.sys.clone(),
        }
    }

    /// The descriptor id of device `device`.
    pub fn device_id(&self, device: &str) -> Option<DeviceId> {
        self.board
            .device(device)
            .and_then(|_| DeviceId::new(descriptor_id(&self.board, device)).ok())
    }

    /// The two generic drivers that bind the board's devices.
    pub fn drivers(&self) -> [L1Driver; 2] {
        [
            self.driver(I2C_DRIVER, InterfaceKind::I2c),
            self.driver(PLATFORM_DRIVER, InterfaceKind::Platform),
        ]
    }

    fn driver(&self, id: &'static str, interface: InterfaceKind) -> L1Driver {
        let board = Arc::clone(&self.board);
        let registry = Arc::clone(&self.registry);
        let sys = self.sys.clone();
        L1Driver::new(
            id,
            "Device from a board definition",
            interface,
            move |descriptor, context| {
                let device = descriptor
                    .properties
                    .get(PROPERTY_DEVICE)
                    .and_then(Value::as_str)
                    .and_then(|device| board.device(device))
                    .ok_or_else(|| DriverError::BindRejected {
                        driver_id: id.into(),
                        device_id: descriptor.id.clone(),
                        reason: "not a device of this board".into(),
                    })?;
                let mut buses = SessionBuses {
                    driver_id: id,
                    context,
                    owner: descriptor,
                    sys: sys.clone(),
                };
                registry
                    .build(device, &mut buses)
                    .map_err(|error| DriverError::Device {
                        driver_id: id.into(),
                        device_id: descriptor.id.clone(),
                        action: format!("build ({error})"),
                        kind: error.kind(),
                    })
            },
        )
    }
}

/// Buses from the runtime's backends, for one device's bind.
struct SessionBuses<'a> {
    driver_id: &'static str,
    context: &'a DriverBindContext<'a>,
    owner: &'a DeviceDescriptor,
    sys: SysRoot,
}

impl Buses for SessionBuses<'_> {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        self.context
            .open_i2c_controller(self.driver_id, self.owner, bus, SessionAccess::Shared)
            .map(|session| DynI2c::new(OwnedI2cBus::new(session)))
            .map_err(|error| BoardError::Device {
                device: self.owner.id.to_string(),
                kind: error.kind(),
                reason: error.to_string(),
            })
    }

    fn sys(&self) -> SysRoot {
        self.sys.clone()
    }
}

fn descriptor_id(board: &BoardDefinition, device: &str) -> String {
    format!("board.{}.{device}", board.board.id)
}

/// Reports a board's devices as configured descriptors.
#[derive(Debug, Clone)]
pub struct BoardProbe {
    board: Arc<BoardDefinition>,
    registry: Arc<DriverRegistry>,
    sys: SysRoot,
}

impl BoardProbe {
    fn descriptor(
        &self,
        spec: &DeviceSpec,
        entry: &DriverEntry,
    ) -> Result<DeviceDescriptor, String> {
        let board = &self.board.board;
        let (interface, driver) = match entry.interface {
            Interface::I2c => (InterfaceKind::I2c, I2C_DRIVER),
            Interface::Platform => (InterfaceKind::Platform, PLATFORM_DRIVER),
            Interface::Composite => {
                return Err("a composite device is hosted by lemnosd, not the runtime".to_owned());
            }
        };
        let mut builder = DeviceDescriptor::builder_for_kind(
            descriptor_id(&self.board, &spec.id),
            DeviceKind::Unspecified(interface),
        )
        .map_err(|e| e.to_string())?
        .display_name(spec.label.clone().unwrap_or_else(|| spec.id.clone()))
        .summary(entry.summary)
        .driver_hint(driver)
        .label("board", board.id.clone())
        .label(PROPERTY_CLASS, entry.class.name())
        .property(PROPERTY_DRIVER, driver)
        .property(PROPERTY_CLASS, entry.class.name())
        .property(PROPERTY_BOARD, board.id.clone())
        .property(PROPERTY_DEVICE, spec.id.clone())
        .property(PROPERTY_BOARD_DRIVER, spec.driver.clone())
        .property(
            "board.backend",
            match spec.backend {
                Backend::Auto => "auto",
                Backend::Userspace => "userspace",
                Backend::Kernel => "kernel",
            },
        );
        if let Some(poll_ms) = spec.poll_ms {
            builder = builder.property("board.poll_ms", u64::from(poll_ms));
        }
        // A bus selector (`i2c:compatible=...`) is resolved under the sysfs
        // root at each refresh, so a renumbered adapter is followed.
        if let Some(bus) = spec.bus.as_ref().and_then(|b| b.i2c_bus(&self.sys)) {
            let bus = bus?;
            let address = spec.address.or(entry.default_address).unwrap_or_default();
            builder = builder
                .address(DeviceAddress::I2cDevice { bus, address })
                .property("bus", u64::from(bus))
                .property("address", u64::from(address));
        }
        builder.build().map_err(|e| e.to_string())
    }
}

impl DiscoveryProbe for BoardProbe {
    fn name(&self) -> &'static str {
        "lemnos-board"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let mut discovery = ProbeDiscovery::default();
        for spec in &self.board.devices {
            let Some(entry) = self.registry.get(&spec.driver) else {
                discovery.notes.push(format!(
                    "device {:?}: unknown driver {:?}",
                    spec.id, spec.driver
                ));
                continue;
            };
            let interface = match entry.interface {
                Interface::I2c => InterfaceKind::I2c,
                Interface::Platform => InterfaceKind::Platform,
                // lemnosd builds composite devices from other devices; the
                // runtime has no such host.
                Interface::Composite => {
                    discovery.notes.push(format!(
                        "device {:?}: a composite device, hosted by lemnosd",
                        spec.id
                    ));
                    continue;
                }
            };
            if !context.wants(interface) {
                continue;
            }
            let descriptor =
                self.descriptor(spec, entry)
                    .map_err(|message| DiscoveryError::ProbeFailed {
                        probe: self.name().into(),
                        message: format!("device {:?}: {message}", spec.id),
                    })?;
            discovery.devices.push(descriptor);
        }
        Ok(discovery)
    }
}
