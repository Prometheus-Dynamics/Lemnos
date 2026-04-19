use crate::{ManifestError, ManifestResult};

pub(crate) fn validate_driver_id(id: &str) -> ManifestResult<()> {
    if id.is_empty() {
        return Err(ManifestError::EmptyDriverId);
    }
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || ['.', '_', '-', ':', '/'].contains(&ch) {
            continue;
        }
        return Err(ManifestError::InvalidDriverId {
            id: id.to_string(),
            invalid: ch,
        });
    }
    Ok(())
}
