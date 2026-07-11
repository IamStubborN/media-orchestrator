#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("subtitle is not valid WEBVTT")]
pub struct SubtitleValidationError;

pub fn validate_webvtt(contents: &[u8]) -> Result<(), SubtitleValidationError> {
    let contents = std::str::from_utf8(contents).map_err(|_| SubtitleValidationError)?;
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    if !contents.starts_with("WEBVTT") || !contents.contains("-->") {
        return Err(SubtitleValidationError);
    }
    Ok(())
}
