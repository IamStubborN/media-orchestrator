use crate::PublicId;

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStateDto {
    Queued,
    Leased,
    Running,
    CancelRequested,
    BlockedStorage,
    Publishing,
    PlexPending,
    NeedsAction,
    Partial,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsActionReasonDto {
    IdentityAmbiguous,
    PlexMismatch,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobSummaryDto {
    pub id: PublicId,
    pub state: JobStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_action_reason: Option<NeedsActionReasonDto>,
}

#[cfg(test)]
mod tests {
    use super::{JobStateDto, JobSummaryDto, NeedsActionReasonDto};
    use crate::PublicId;

    #[test]
    fn job_summary_has_stable_json_names() {
        let dto = JobSummaryDto {
            id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
            state: JobStateDto::NeedsAction,
            needs_action_reason: Some(NeedsActionReasonDto::PlexMismatch),
        };

        let value = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "state": "needs_action",
                "needs_action_reason": "plex_mismatch"
            }),
        );
        assert_eq!(serde_json::from_value::<JobSummaryDto>(value).unwrap(), dto,);
    }

    #[test]
    fn job_summary_omits_absent_action_reason() {
        let dto = JobSummaryDto {
            id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
            state: JobStateDto::Queued,
            needs_action_reason: None,
        };

        assert_eq!(
            serde_json::to_value(dto).unwrap(),
            serde_json::json!({
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "state": "queued"
            }),
        );
    }
}
