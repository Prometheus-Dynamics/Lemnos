use super::*;

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn refusal<'a>(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Option<ConfigError<'a>> {
    <Indicator as lemnos_core::Lifecycle>::check_config(&Config::new(entries)).err()
}

#[test]
fn check_config_accepts_a_full_config() {
    let entries = [
        ("brightness", ConfigValue::Int(128)),
        ("state.failed", ConfigValue::Str("blink #ff0000 period=250")),
        ("state.writing", ConfigValue::Str("bar #00c040 off=#001000")),
    ];
    assert_eq!(refusal(&entries), None);
}

#[test]
fn check_config_refuses_an_unknown_key_and_suggests_the_near_one() {
    let err = refusal(&[("state.faild", ConfigValue::Str("off"))]).unwrap();
    assert_eq!(err.key, "state.faild");
    assert_eq!(err.suggestion, Some("state.failed"));
    assert_eq!(
        refusal(&[("bogus", ConfigValue::Int(1))]).unwrap().key,
        "bogus"
    );
}

#[test]
fn check_config_refuses_a_wrong_kind() {
    let err = refusal(&[("brightness", ConfigValue::Str("128"))]).unwrap();
    assert_eq!(err.key, "brightness");
}

#[test]
fn check_config_keeps_brightness_to_a_byte() {
    assert_eq!(
        refusal(&[("brightness", ConfigValue::Int(256))])
            .unwrap()
            .key,
        "brightness"
    );
    assert_eq!(
        refusal(&[("brightness", ConfigValue::Int(-1))])
            .unwrap()
            .key,
        "brightness"
    );
    assert_eq!(refusal(&[("brightness", ConfigValue::Int(0))]), None);
    assert_eq!(refusal(&[("brightness", ConfigValue::Int(255))]), None);
}

#[test]
fn check_config_refuses_a_pattern_that_does_not_parse() {
    let fast = [("state.failed", ConfigValue::Str("blink #ff0000 period=50"))];
    let err = refusal(&fast).unwrap();
    assert_eq!(err.key, "state.failed");
    let edge = [("state.failed", ConfigValue::Str("blink #ff0000 period=100"))];
    assert_eq!(refusal(&edge), None);
}
