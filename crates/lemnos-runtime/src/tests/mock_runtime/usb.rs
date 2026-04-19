use super::super::support::{
    mock_usb_composite_device, mock_usb_device, usb_vendor_request,
    usb_vendor_request_for_interface,
};
use crate::{Runtime, RuntimeFailureCategory, RuntimeFailureOperation};
use lemnos_bus::BusError;
use lemnos_core::{
    DeviceRequest, InteractionRequest, StandardRequest, StandardResponse, UsbRequest, UsbResponse,
};
use lemnos_discovery::DiscoveryContext;
use lemnos_driver_sdk::DriverError;
use lemnos_drivers_usb::UsbDriver;
use lemnos_mock::{MockFaultScript, MockHardware};

#[test]
fn runtime_dispatches_usb_requests_and_tracks_state() {
    let hardware = MockHardware::builder()
        .with_usb_device(mock_usb_device())
        .build();
    let interface_id = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(hardware.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let claim_response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            })),
        ))
        .expect("claim request");
    assert_eq!(
        claim_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(
            UsbResponse::InterfaceClaimed {
                interface_number: 0,
                alternate_setting: Some(1),
            }
        ))
    );

    let response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request(),
            ))),
        ))
        .expect("control request");

    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(
            vec![0x10, 0x20, 0x30, 0x40]
        )))
    );
    assert!(runtime.is_bound(&interface_id));
    assert_eq!(
        runtime
            .state(&interface_id)
            .expect("state")
            .realized_config
            .get("interface_number"),
        Some(&0_u64.into())
    );
    assert_eq!(
        runtime
            .state(&interface_id)
            .expect("state")
            .telemetry
            .get("control_ops"),
        Some(&1_u64.into())
    );
}

#[test]
fn runtime_rebinds_after_mock_usb_hotplug_cycle() {
    let usb = mock_usb_device();
    let hardware = MockHardware::builder().with_usb_device(usb.clone()).build();
    let descriptors = hardware.descriptors();
    let interface_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface")
        .id
        .clone();
    let device_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbDevice)
        .expect("usb device")
        .id
        .clone();

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(hardware.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    assert!(runtime.inventory().contains(&interface_id));
    assert!(runtime.inventory().contains(&device_id));

    runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            })),
        ))
        .expect("initial claim");
    assert!(runtime.is_bound(&interface_id));
    assert_eq!(hardware.usb_claimed_interfaces(&device_id), Some(vec![0]));

    assert!(hardware.remove_device(&interface_id));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after usb removal");

    assert!(!runtime.inventory().contains(&interface_id));
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&interface_id));
    assert!(runtime.state(&interface_id).is_none());

    hardware.attach_usb_device(usb);
    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after usb reattach");
    assert_eq!(report.rebinds.attempted, vec![interface_id.clone()]);
    assert_eq!(report.rebinds.rebound, vec![interface_id.clone()]);
    assert!(runtime.inventory().contains(&interface_id));
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.is_bound(&interface_id));

    let claim_response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: None,
            })),
        ))
        .expect("claim after reattach");
    assert_eq!(
        claim_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(
            UsbResponse::InterfaceClaimed {
                interface_number: 0,
                alternate_setting: None,
            }
        ))
    );

    let response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request(),
            ))),
        ))
        .expect("control after reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(
            vec![0x10, 0x20, 0x30, 0x40]
        )))
    );
    assert!(runtime.is_bound(&interface_id));
    assert_eq!(hardware.usb_claimed_interfaces(&device_id), Some(vec![0]));
}

#[test]
fn runtime_tracks_and_clears_mock_usb_request_failures() {
    let hardware = MockHardware::builder()
        .with_usb_device(mock_usb_device())
        .build();
    let descriptors = hardware.descriptors();
    let interface_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface")
        .id
        .clone();
    let device_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbDevice)
        .expect("usb device")
        .id
        .clone();

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(hardware.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    hardware.queue_script(
        &interface_id,
        MockFaultScript::new()
            .timeout("usb.claim_interface")
            .disconnect("usb.control_transfer"),
    );

    let claim_err = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            })),
        ))
        .expect_err("first claim should fail");
    assert!(matches!(
        claim_err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == interface_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Timeout {
                        operation: "usb.claim_interface",
                        ..
                    },
                    ..
                }
            )
    ));
    assert!(runtime.is_bound(&interface_id));
    let failure = runtime
        .failure(&interface_id)
        .expect("claim failure should be tracked");
    assert_eq!(failure.operation, RuntimeFailureOperation::Request);
    assert_eq!(failure.category, RuntimeFailureCategory::Driver);

    let claim_response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            })),
        ))
        .expect("second claim should succeed");
    assert_eq!(
        claim_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(
            UsbResponse::InterfaceClaimed {
                interface_number: 0,
                alternate_setting: Some(1),
            }
        ))
    );
    assert_eq!(hardware.usb_claimed_interfaces(&device_id), Some(vec![0]));
    assert!(runtime.failure(&interface_id).is_none());

    let control_err = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request(),
            ))),
        ))
        .expect_err("first control should hit scripted disconnect");
    assert!(matches!(
        control_err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == interface_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Disconnected { .. },
                    ..
                }
            )
    ));
    assert!(runtime.is_bound(&interface_id));
    assert_eq!(
        runtime
            .failure(&interface_id)
            .expect("control failure should be tracked")
            .operation,
        RuntimeFailureOperation::Request
    );

    let response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request(),
            ))),
        ))
        .expect("second control should succeed");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(
            vec![0x10, 0x20, 0x30, 0x40]
        )))
    );
    assert!(runtime.failure(&interface_id).is_none());
}

#[test]
fn runtime_rebinds_composite_usb_interfaces_after_owner_removal() {
    let usb = mock_usb_composite_device();
    let hardware = MockHardware::builder().with_usb_device(usb.clone()).build();
    let descriptors = hardware.descriptors();
    let mut interface_ids = descriptors
        .iter()
        .filter(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .map(|device| device.id.clone())
        .collect::<Vec<_>>();
    interface_ids.sort();
    let device_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbDevice)
        .expect("usb device")
        .id
        .clone();

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(hardware.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    assert_eq!(interface_ids.len(), 2);
    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.inventory().contains(&interface_ids[0]));
    assert!(runtime.inventory().contains(&interface_ids[1]));

    for (index, interface_id) in interface_ids.iter().enumerate() {
        runtime
            .request(DeviceRequest::new(
                interface_id.clone(),
                InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                    interface_number: index as u8,
                    alternate_setting: Some(index as u8),
                })),
            ))
            .expect("claim composite interface");
    }

    assert!(runtime.is_bound(&interface_ids[0]));
    assert!(runtime.is_bound(&interface_ids[1]));
    assert_eq!(
        hardware.usb_claimed_interfaces(&device_id),
        Some(vec![0, 1])
    );

    assert!(hardware.remove_device(&interface_ids[0]));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after composite removal");

    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.inventory().contains(&interface_ids[0]));
    assert!(!runtime.inventory().contains(&interface_ids[1]));
    assert!(!runtime.is_bound(&interface_ids[0]));
    assert!(!runtime.is_bound(&interface_ids[1]));
    assert!(runtime.state(&interface_ids[0]).is_none());
    assert!(runtime.state(&interface_ids[1]).is_none());

    hardware.attach_usb_device(usb);
    let report = runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after composite reattach");
    assert_eq!(report.rebinds.attempted, interface_ids.clone());
    assert_eq!(report.rebinds.rebound, interface_ids.clone());

    assert!(runtime.inventory().contains(&device_id));
    assert!(runtime.inventory().contains(&interface_ids[0]));
    assert!(runtime.inventory().contains(&interface_ids[1]));
    assert!(runtime.is_bound(&interface_ids[0]));
    assert!(runtime.is_bound(&interface_ids[1]));

    let response = runtime
        .request(DeviceRequest::new(
            interface_ids[1].clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::Control(
                usb_vendor_request_for_interface(1),
            ))),
        ))
        .expect("control after composite reattach");
    assert_eq!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::Bytes(
            vec![0x41, 0x42, 0x43, 0x44]
        )))
    );
    assert!(runtime.is_bound(&interface_ids[1]));
}

#[test]
fn runtime_clears_usb_release_failure_after_hotplug_reset() {
    let usb = mock_usb_device();
    let hardware = MockHardware::builder().with_usb_device(usb.clone()).build();
    let descriptors = hardware.descriptors();
    let interface_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbInterface)
        .expect("usb interface")
        .id
        .clone();
    let device_id = descriptors
        .iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::UsbDevice)
        .expect("usb device")
        .id
        .clone();

    let mut runtime = Runtime::new();
    runtime.set_usb_backend(hardware.clone());
    runtime.register_driver(UsbDriver).expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: Some(1),
            })),
        ))
        .expect("initial claim");
    assert_eq!(hardware.usb_claimed_interfaces(&device_id), Some(vec![0]));

    hardware.queue_disconnect(&interface_id, "usb.release_interface");

    let err = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ReleaseInterface {
                interface_number: 0,
            })),
        ))
        .expect_err("release should fail before hotplug reset");
    assert!(matches!(
        err,
        crate::RuntimeError::Driver {
            device_id: failing_device_id,
            source,
        } if failing_device_id == interface_id
            && matches!(
                source.as_ref(),
                DriverError::Transport {
                    source: BusError::Disconnected { .. },
                    ..
                }
            )
    ));
    assert_eq!(hardware.usb_claimed_interfaces(&device_id), Some(vec![0]));
    assert_eq!(
        runtime
            .failure(&interface_id)
            .expect("release failure should be tracked")
            .operation,
        RuntimeFailureOperation::Request
    );

    assert!(hardware.remove_device(&interface_id));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after removal");
    assert!(!runtime.inventory().contains(&interface_id));
    assert!(!runtime.inventory().contains(&device_id));
    assert!(!runtime.is_bound(&interface_id));
    assert!(runtime.failure(&interface_id).is_none());

    hardware.attach_usb_device(usb);
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after reattach");

    let claim_response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ClaimInterface {
                interface_number: 0,
                alternate_setting: None,
            })),
        ))
        .expect("claim after release-reset reattach");
    assert_eq!(
        claim_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(
            UsbResponse::InterfaceClaimed {
                interface_number: 0,
                alternate_setting: None,
            }
        ))
    );

    let release_response = runtime
        .request(DeviceRequest::new(
            interface_id.clone(),
            InteractionRequest::Standard(StandardRequest::Usb(UsbRequest::ReleaseInterface {
                interface_number: 0,
            })),
        ))
        .expect("release after reattach");
    assert_eq!(
        release_response.interaction,
        lemnos_core::InteractionResponse::Standard(StandardResponse::Usb(
            UsbResponse::InterfaceReleased {
                interface_number: 0,
            }
        ))
    );
    assert_eq!(
        hardware.usb_claimed_interfaces(&device_id),
        Some(Vec::new())
    );
    assert!(runtime.failure(&interface_id).is_none());
}
