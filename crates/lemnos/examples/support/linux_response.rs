use lemnos::core::{DeviceResponse, InteractionResponse, Value};

pub fn custom_output_field<'a>(response: &'a DeviceResponse, key: &str) -> Option<&'a Value> {
    match &response.interaction {
        InteractionResponse::Custom(lemnos::core::CustomInteractionResponse {
            output: Some(Value::Map(map)),
            ..
        }) => map.get(key),
        _ => None,
    }
}
