use lemnos::prelude::{GpioDirection, GpioLevel, GpioLineConfiguration};

pub fn output_config() -> GpioLineConfiguration {
    GpioLineConfiguration {
        direction: GpioDirection::Output,
        active_low: false,
        bias: None,
        drive: None,
        edge: None,
        debounce_us: None,
        initial_level: Some(GpioLevel::Low),
    }
}
