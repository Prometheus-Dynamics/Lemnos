use lemnos_core::PwmPolarity;

pub(crate) fn polarity_name(polarity: PwmPolarity) -> &'static str {
    match polarity {
        PwmPolarity::Normal => "normal",
        PwmPolarity::Inversed => "inversed",
    }
}
