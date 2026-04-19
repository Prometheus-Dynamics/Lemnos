use lemnos_driver_sdk::pwm;

lemnos_driver_sdk::define_generic_driver_manifest! {
    id: "lemnos.pwm.generic";
    summary: "Generic PWM driver bundle";
    interface: lemnos_core::InterfaceKind::Pwm;
    kind: lemnos_core::DeviceKind::PwmChannel;
    interactions: &[
        (pwm::ENABLE_INTERACTION, "Enable or disable PWM output"),
        (pwm::CONFIGURE_INTERACTION, "Configure PWM timing and polarity"),
        (pwm::SET_PERIOD_INTERACTION, "Set PWM period"),
        (pwm::SET_DUTY_CYCLE_INTERACTION, "Set PWM duty cycle"),
        (
            pwm::GET_CONFIGURATION_INTERACTION,
            "Read active PWM configuration",
        ),
    ];
}
