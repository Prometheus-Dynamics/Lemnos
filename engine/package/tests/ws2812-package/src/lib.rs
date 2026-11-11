//! The `ws2812` driver as a package. Nothing here but the export: the driver is the merged
//! crate, as it is built into the service.

#![forbid(unsafe_code)]

lemnos_core::export_package!(lemnos_driver_ws2812::Ws2812);
