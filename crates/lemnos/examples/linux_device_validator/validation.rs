use crate::inventory::{
    find_gpio_line, find_hwmon_fan, find_led, find_spi_device, find_uart_port, find_usb_interface,
};
use crate::support::board_validator::config::{
    FanTarget, GpioReadTarget, LedTarget, SpiTarget, UsbInterfaceTarget, ValidatorConfig,
};
use crate::support::board_validator::drivers::{Bmi055Config, Bmm150Config, PowerSensorConfig};
use crate::support::board_validator::report::ValidatorReport;
use crate::{linux_hwmon_fan, linux_led};
use lemnos::core::{
    DeviceId, DeviceResponse, GpioResponse, InteractionResponse, StandardResponse, UsbResponse,
    Value,
};
use lemnos::prelude::*;

pub fn run_validations(
    lemnos: &mut Lemnos,
    config: &ValidatorConfig,
    bmi055_config: Option<&Bmi055Config>,
    bmm150_config: Option<&Bmm150Config>,
    power_config: Option<&PowerSensorConfig>,
    report: &mut ValidatorReport,
) {
    if config.gpio_reads.is_empty() {
        report.skip("gpio.read", "no GPIO_READ_LINES configured");
    } else {
        for target in &config.gpio_reads {
            validate_gpio_line(lemnos, target, report);
        }
    }

    if config.leds.is_empty() {
        report.skip("led.class", "no LED_TARGETS configured");
    } else {
        for target in &config.leds {
            validate_led(lemnos, target, report);
        }
    }

    if config.fans.is_empty() {
        report.skip("fan.hwmon", "no FAN_TARGETS configured");
    } else {
        for target in &config.fans {
            validate_fan(lemnos, target, report);
        }
    }

    if config.spis.is_empty() {
        report.skip("spi.configuration", "no SPI_TARGETS configured");
    } else {
        for target in &config.spis {
            validate_spi_device(lemnos, target, report);
        }
    }

    if config.uart_ports.is_empty() {
        report.skip("uart.configuration", "no UART_PORTS configured");
    } else {
        for port in &config.uart_ports {
            validate_uart_port(lemnos, port, report);
        }
    }

    if let Some(target) = &config.usb_target {
        validate_usb_interface(lemnos, target, report);
    } else {
        report.skip("usb.interface", "no USB_TARGET configured");
    }

    if let Some(config) = bmi055_config {
        validate_sensor_device(
            lemnos,
            &config.logical_device_id().expect("validated BMI055 config"),
            "bosch_imu.sample",
            "sensor.imu.sample",
            report,
        );
    } else {
        report.skip(
            "bosch_imu.sample",
            "no BMI088_* or BMI055_* target configured",
        );
    }

    if let Some(config) = bmm150_config {
        validate_sensor_device(
            lemnos,
            &config.logical_device_id().expect("validated BMM150 config"),
            "bmm150.sample",
            "sensor.magnetometer.sample",
            report,
        );
    } else {
        report.skip("bmm150.sample", "no BMM150_* target configured");
    }

    if let Some(config) = power_config {
        validate_sensor_device(
            lemnos,
            &config.logical_device_id().expect("validated power config"),
            "power.sample",
            "sensor.power.sample",
            report,
        );
    } else {
        report.skip("power.sample", "no POWER_* target configured");
    }

    if let Some(target) = config
        .bmi055
        .as_ref()
        .and_then(|target| target.accel_interrupt.as_ref())
    {
        validate_gpio_line(lemnos, target, report);
    }

    if let Some(target) = config
        .bmi055
        .as_ref()
        .and_then(|target| target.gyro_interrupt.as_ref())
    {
        validate_gpio_line(lemnos, target, report);
    }
}

fn validate_gpio_line(lemnos: &mut Lemnos, target: &GpioReadTarget, report: &mut ValidatorReport) {
    let check_name = format!("gpio.read {}:{}", target.chip_name, target.offset);
    let Some(device_id) = find_gpio_line(lemnos.inventory(), target) else {
        report.fail(check_name, "GPIO line not present in inventory");
        return;
    };

    match lemnos.request_gpio(device_id.clone(), GpioRequest::GetConfiguration) {
        Ok(response) => {
            let configuration = describe_standard_response(&response);
            match lemnos.request_gpio(device_id, GpioRequest::Read) {
                Ok(response) => {
                    let detail = describe_gpio_level(&response);
                    if let Some(expected_level) = target.expected_level
                        && !gpio_response_matches(&response, expected_level)
                    {
                        report.fail(
                            check_name,
                            format!(
                                "{configuration}; expected {:?}, got {detail}",
                                expected_level
                            ),
                        );
                        return;
                    }
                    report.pass(check_name, format!("{configuration}; {detail}"));
                }
                Err(error) => {
                    report.fail(check_name, format!("{configuration}; read failed: {error}"))
                }
            }
        }
        Err(error) => report.fail(check_name, format!("get configuration failed: {error}")),
    }
}

fn validate_uart_port(lemnos: &mut Lemnos, port: &str, report: &mut ValidatorReport) {
    let check_name = format!("uart.configuration {port}");
    let Some(device_id) = find_uart_port(lemnos.inventory(), port) else {
        report.fail(check_name, "UART port not present in inventory");
        return;
    };

    match lemnos.request_uart(device_id, UartRequest::GetConfiguration) {
        Ok(response) => report.pass(check_name, describe_standard_response(&response)),
        Err(error) => report.fail(check_name, error.to_string()),
    }
}

fn validate_led(lemnos: &mut Lemnos, target: &LedTarget, report: &mut ValidatorReport) {
    let check_name = format!("led.class {}", target.name);
    let Some(device_id) = find_led(lemnos.inventory(), &target.name) else {
        report.fail(check_name, "LED not present in inventory");
        return;
    };

    if let Err(error) = lemnos.bind(&device_id) {
        report.fail(check_name, format!("bind failed: {error}"));
        return;
    }

    let read = match lemnos.request_custom(device_id.clone(), linux_led::LED_READ_INTERACTION) {
        Ok(response) => response,
        Err(error) => {
            report.fail(check_name, format!("read failed: {error}"));
            return;
        }
    };

    if let Some(expected_trigger) = &target.expect_trigger {
        let actual_trigger = custom_output_field(&read, "active_trigger").and_then(Value::as_str);
        if actual_trigger != Some(expected_trigger.as_str()) {
            report.fail(
                check_name,
                format!(
                    "expected trigger '{}', got {:?}",
                    expected_trigger, actual_trigger
                ),
            );
            return;
        }
    }

    report.pass(check_name, describe_response(&read));
}

fn validate_fan(lemnos: &mut Lemnos, target: &FanTarget, report: &mut ValidatorReport) {
    let check_name = format!("fan.hwmon {}", target.hwmon_name);
    let Some(device_id) = find_hwmon_fan(lemnos.inventory(), &target.hwmon_name) else {
        report.fail(check_name, "hwmon fan device not present in inventory");
        return;
    };

    if let Err(error) = lemnos.bind(&device_id) {
        report.fail(check_name, format!("bind failed: {error}"));
        return;
    }

    let read = match lemnos.request_custom(device_id.clone(), linux_hwmon_fan::FAN_READ_INTERACTION)
    {
        Ok(response) => response,
        Err(error) => {
            report.fail(check_name, format!("read failed: {error}"));
            return;
        }
    };

    let mut detail = vec![describe_response(&read)];

    if let Some(pwm) = target.set_pwm {
        match lemnos.request_custom_value(
            device_id.clone(),
            linux_hwmon_fan::FAN_SET_PWM_INTERACTION,
            pwm,
        ) {
            Ok(response) => detail.push(describe_response(&response)),
            Err(error) => {
                report.fail(check_name, format!("set pwm failed: {error}"));
                return;
            }
        }
    }

    if let Some(mode) = target.set_mode {
        match lemnos.request_custom_value(
            device_id.clone(),
            linux_hwmon_fan::FAN_SET_MODE_INTERACTION,
            mode,
        ) {
            Ok(response) => detail.push(describe_response(&response)),
            Err(error) => {
                report.fail(check_name, format!("set mode failed: {error}"));
                return;
            }
        }
    }

    report.pass(check_name, detail.join("; "));
}

fn validate_spi_device(lemnos: &mut Lemnos, target: &SpiTarget, report: &mut ValidatorReport) {
    let check_name = format!("spi.configuration {}:{}", target.bus, target.chip_select);
    let Some(device_id) = find_spi_device(lemnos.inventory(), target) else {
        report.fail(check_name, "SPI device not present in inventory");
        return;
    };

    match lemnos.request_spi(device_id, SpiRequest::GetConfiguration) {
        Ok(response) => report.pass(check_name, describe_standard_response(&response)),
        Err(error) => report.fail(check_name, error.to_string()),
    }
}

fn validate_usb_interface(
    lemnos: &mut Lemnos,
    target: &UsbInterfaceTarget,
    report: &mut ValidatorReport,
) {
    let ports = target
        .ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let check_name = format!(
        "usb.interface {}:{}:{}",
        target.bus, ports, target.interface_number
    );
    let Some(device_id) = find_usb_interface(lemnos.inventory(), target) else {
        report.fail(check_name, "USB interface not present in inventory");
        return;
    };

    match lemnos.request_usb(
        device_id.clone(),
        UsbRequest::ClaimInterface {
            interface_number: target.interface_number,
            alternate_setting: target.alternate_setting,
        },
    ) {
        Ok(claim) => match lemnos.request_usb(
            device_id,
            UsbRequest::ReleaseInterface {
                interface_number: target.interface_number,
            },
        ) {
            Ok(release) => report.pass(
                check_name,
                format!(
                    "{}; {}",
                    describe_standard_response(&claim),
                    describe_standard_response(&release)
                ),
            ),
            Err(error) => report.fail(check_name, format!("claim ok; release failed: {error}")),
        },
        Err(error) => report.fail(check_name, error.to_string()),
    }
}

fn validate_sensor_device(
    lemnos: &mut Lemnos,
    device_id: &DeviceId,
    check_name: &str,
    interaction: &str,
    report: &mut ValidatorReport,
) {
    if let Err(error) = lemnos.bind(device_id) {
        report.fail(check_name, format!("bind failed: {error}"));
        return;
    }

    match lemnos.request_custom(device_id.clone(), interaction) {
        Ok(response) => {
            let interaction_summary = describe_response(&response);
            match lemnos.refresh_state(device_id) {
                Ok(Some(state)) => report.pass(
                    check_name,
                    format!(
                        "{interaction_summary}; telemetry keys={}",
                        state
                            .telemetry
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                ),
                Ok(None) => report.pass(
                    check_name,
                    format!("{interaction_summary}; no cached state"),
                ),
                Err(error) => report.fail(
                    check_name,
                    format!("{interaction_summary}; state refresh failed: {error}"),
                ),
            }
        }
        Err(error) => report.fail(check_name, error.to_string()),
    }
}

fn describe_response(response: &DeviceResponse) -> String {
    match &response.interaction {
        InteractionResponse::Standard(_) => describe_standard_response(response),
        InteractionResponse::Custom(custom) => {
            format!(
                "custom interaction {} output={:?}",
                custom.id, custom.output
            )
        }
    }
}

fn custom_output_field<'a>(response: &'a DeviceResponse, key: &str) -> Option<&'a Value> {
    match &response.interaction {
        InteractionResponse::Custom(lemnos::core::CustomInteractionResponse {
            output: Some(Value::Map(map)),
            ..
        }) => map.get(key),
        _ => None,
    }
}

fn describe_standard_response(response: &DeviceResponse) -> String {
    match &response.interaction {
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Configuration(
            configuration,
        ))) => {
            format!(
                "gpio configuration direction={:?} active_low={} edge={:?}",
                configuration.direction, configuration.active_low, configuration.edge
            )
        }
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Level(level))) => {
            format!("gpio level={:?}", level)
        }
        InteractionResponse::Standard(StandardResponse::Uart(response)) => {
            format!("uart response={response:?}")
        }
        InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::InterfaceClaimed {
            interface_number,
            alternate_setting,
        })) => format!(
            "usb interface claimed interface={} alternate_setting={alternate_setting:?}",
            interface_number
        ),
        InteractionResponse::Standard(StandardResponse::Usb(UsbResponse::InterfaceReleased {
            interface_number,
        })) => format!("usb interface released interface={interface_number}"),
        InteractionResponse::Standard(other) => format!("standard response={other:?}"),
        InteractionResponse::Custom(custom) => {
            format!(
                "custom interaction {} output={:?}",
                custom.id, custom.output
            )
        }
    }
}

fn describe_gpio_level(response: &DeviceResponse) -> String {
    match &response.interaction {
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Level(level))) => {
            format!("gpio level={level:?}")
        }
        _ => describe_standard_response(response),
    }
}

fn gpio_response_matches(response: &DeviceResponse, expected_level: GpioLevel) -> bool {
    matches!(
        &response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Level(level)))
            if *level == expected_level
    )
}
