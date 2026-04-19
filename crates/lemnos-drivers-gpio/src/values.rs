use lemnos_core::{GpioBias, GpioDirection, GpioDrive, GpioEdge, GpioLevel, Value};

pub(crate) fn direction_name(direction: GpioDirection) -> Value {
    match direction {
        GpioDirection::Input => "input".into(),
        GpioDirection::Output => "output".into(),
    }
}

pub(crate) fn level_name(level: GpioLevel) -> Value {
    match level {
        GpioLevel::Low => "low".into(),
        GpioLevel::High => "high".into(),
    }
}

pub(crate) fn bias_name(bias: GpioBias) -> Value {
    match bias {
        GpioBias::Disabled => "disabled".into(),
        GpioBias::PullUp => "pull-up".into(),
        GpioBias::PullDown => "pull-down".into(),
    }
}

pub(crate) fn drive_name(drive: GpioDrive) -> Value {
    match drive {
        GpioDrive::PushPull => "push-pull".into(),
        GpioDrive::OpenDrain => "open-drain".into(),
        GpioDrive::OpenSource => "open-source".into(),
    }
}

pub(crate) fn edge_name(edge: GpioEdge) -> Value {
    match edge {
        GpioEdge::Rising => "rising".into(),
        GpioEdge::Falling => "falling".into(),
        GpioEdge::Both => "both".into(),
    }
}
