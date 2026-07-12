#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ResumeAction {
    Append,
    Restart,
    /// The partial already spans the whole resource; nothing remains to fetch.
    Complete,
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
        // 416 Range Not Satisfiable: the server rejects the requested start.
        // When the partial already equals the resource length it is complete
        // rather than an error, so a prior attempt that stopped after the full
        // download can finish without re-fetching.
        (existing, 416, Some((_, Some(total)))) if existing > 0 && total == existing => {
            Ok(ResumeAction::Complete)
        }
        _ => Err(ResumeError),
    }
}
