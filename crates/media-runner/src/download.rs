#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ResumeAction {
    Append,
    Restart,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("HTTP range response is incompatible with the partial file")]
pub struct ResumeError;

pub fn decide_resume(
    existing_bytes: u64,
    status: u16,
    content_range: Option<(u64, Option<u64>)>,
) -> Result<ResumeAction, ResumeError> {
    match (existing_bytes, status, content_range) {
        (0, 200, None) => Ok(ResumeAction::Restart),
        (existing, 206, Some((start, total))) if start == existing && total != Some(0) => {
            Ok(ResumeAction::Append)
        }
        (existing, 200, None) if existing > 0 => Ok(ResumeAction::Restart),
        _ => Err(ResumeError),
    }
}
