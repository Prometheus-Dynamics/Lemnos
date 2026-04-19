use crate::PwmDriver;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InteractionRequest, InteractionResponse, PwmConfiguration,
    PwmPolarity, PwmRequest, PwmResponse, StandardRequest, StandardResponse, Value,
};
use lemnos_driver_sdk::{Driver, DriverBindContext, pwm};
use lemnos_mock::{MockHardware, MockPwmChannel};

fn mock_channel() -> (MockHardware, DeviceDescriptor) {
    let hardware = MockHardware::builder()
        .with_pwm_channel(
            MockPwmChannel::new("pwmchip0", 1)
                .with_display_name("fan-output")
                .with_configuration(PwmConfiguration {
                    period_ns: 20_000_000,
                    duty_cycle_ns: 2_500_000,
                    enabled: false,
                    polarity: PwmPolarity::Normal,
                }),
        )
        .build();
    let device = hardware
        .descriptors()
        .into_iter()
        .find(|device| device.kind == DeviceKind::PwmChannel)
        .expect("pwm channel");
    (hardware, device)
}

#[test]
fn binds_and_handles_pwm_requests() {
    let (hardware, device) = mock_channel();
    let mut bound = PwmDriver
        .bind(&device, &DriverBindContext::default().with_pwm(&hardware))
        .expect("bind");

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Pwm(
            PwmRequest::Configure(PwmConfiguration {
                period_ns: 25_000_000,
                duty_cycle_ns: 5_000_000,
                enabled: true,
                polarity: PwmPolarity::Inversed,
            }),
        )))
        .expect("configure");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Pwm(PwmResponse::Applied))
    );

    let response = bound
        .execute(&InteractionRequest::Standard(StandardRequest::Pwm(
            PwmRequest::GetConfiguration,
        )))
        .expect("get configuration");
    assert_eq!(
        response,
        InteractionResponse::Standard(StandardResponse::Pwm(PwmResponse::Configuration(
            PwmConfiguration {
                period_ns: 25_000_000,
                duty_cycle_ns: 5_000_000,
                enabled: true,
                polarity: PwmPolarity::Inversed,
            }
        )))
    );
}

#[test]
fn state_reports_pwm_configuration_and_ratio() {
    let (hardware, device) = mock_channel();
    let mut bound = PwmDriver
        .bind(&device, &DriverBindContext::default().with_pwm(&hardware))
        .expect("bind");

    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Pwm(
            PwmRequest::Enable { enabled: true },
        )))
        .expect("enable");
    bound
        .execute(&InteractionRequest::Standard(StandardRequest::Pwm(
            PwmRequest::SetDutyCycle {
                duty_cycle_ns: 10_000_000,
            },
        )))
        .expect("set duty cycle");

    let state = bound
        .state()
        .expect("state")
        .expect("snapshot should exist");

    assert_eq!(
        state.realized_config.get("enabled"),
        Some(&Value::from(true))
    );
    assert_eq!(
        state.realized_config.get("polarity"),
        Some(&Value::from("normal"))
    );
    assert_eq!(
        state.telemetry.get("duty_cycle_ratio"),
        Some(&Value::from(0.5_f64))
    );
    assert_eq!(state.telemetry.get("enable_ops"), Some(&Value::from(1_u64)));
    assert_eq!(
        state.telemetry.get("configure_ops"),
        Some(&Value::from(0_u64))
    );
    assert_eq!(
        state.telemetry.get("set_period_ops"),
        Some(&Value::from(0_u64))
    );
    assert_eq!(
        state.telemetry.get("set_duty_cycle_ops"),
        Some(&Value::from(1_u64))
    );
    assert_eq!(
        state
            .last_operation
            .as_ref()
            .map(|operation| operation.interaction.as_str()),
        Some(pwm::SET_DUTY_CYCLE_INTERACTION)
    );
}
