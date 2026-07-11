use crate::{NotifyScopeDto, ProviderDto, PublicId};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CreateJobRequest {
    pub provider: ProviderDto,
    pub result_ref: String,
    pub notify_scope: NotifyScopeDto,
}

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
#[serde(rename_all = "snake_case")]
pub struct JobDto {
    pub id: PublicId,
    pub provider: ProviderDto,
    pub result_ref: String,
    pub state: JobStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_action_reason: Option<NeedsActionReasonDto>,
    pub notify_scope: NotifyScopeDto,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct QueueStatusDto {
    pub queued: u64,
    pub active: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobSummaryDto {
    pub id: PublicId,
    pub state: JobStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_action_reason: Option<NeedsActionReasonDto>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JobListDto {
    pub jobs: Vec<JobDto>,
}

#[cfg(test)]
mod tests {
    use super::{
        CreateJobRequest, JobDto, JobStateDto, JobSummaryDto, NeedsActionReasonDto, QueueStatusDto,
    };
    use crate::{NotifyScopeDto, ProviderDto, PublicId};

    #[test]
    fn job_enums_use_stable_snake_case_names() {
        let states = [
            (JobStateDto::Queued, "queued"),
            (JobStateDto::Leased, "leased"),
            (JobStateDto::Running, "running"),
            (JobStateDto::CancelRequested, "cancel_requested"),
            (JobStateDto::BlockedStorage, "blocked_storage"),
            (JobStateDto::Publishing, "publishing"),
            (JobStateDto::PlexPending, "plex_pending"),
            (JobStateDto::NeedsAction, "needs_action"),
            (JobStateDto::Partial, "partial"),
            (JobStateDto::Completed, "completed"),
            (JobStateDto::Failed, "failed"),
            (JobStateDto::Cancelled, "cancelled"),
        ];
        for (value, name) in states {
            let json = format!("\"{name}\"");
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(serde_json::from_str::<JobStateDto>(&json).unwrap(), value);
        }

        let reasons = [
            (
                NeedsActionReasonDto::IdentityAmbiguous,
                "identity_ambiguous",
            ),
            (NeedsActionReasonDto::PlexMismatch, "plex_mismatch"),
        ];
        for (value, name) in reasons {
            let json = format!("\"{name}\"");
            assert_eq!(serde_json::to_string(&value).unwrap(), json);
            assert_eq!(
                serde_json::from_str::<NeedsActionReasonDto>(&json).unwrap(),
                value,
            );
        }
    }

    #[test]
    fn create_job_request_has_a_stable_public_shape_and_round_trips() {
        let request = CreateJobRequest {
            provider: ProviderDto::Rezka,
            result_ref: "rezka:series:42:season:1".to_owned(),
            notify_scope: NotifyScopeDto::Initiator,
        };

        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "provider": "rezka",
                "result_ref": "rezka:series:42:season:1",
                "notify_scope": "initiator"
            }),
        );
        assert_eq!(
            serde_json::from_value::<CreateJobRequest>(value).unwrap(),
            request,
        );
    }

    #[test]
    fn create_job_request_rejects_identity_and_other_unknown_fields() {
        for forbidden_field in ["owner_id", "requested_by", "unexpected"] {
            let mut value = serde_json::json!({
                "provider": "prowlarr",
                "result_ref": "prowlarr:result:7",
                "notify_scope": "family"
            });
            value.as_object_mut().unwrap().insert(
                forbidden_field.to_owned(),
                serde_json::Value::String("not-allowed".to_owned()),
            );

            let error = serde_json::from_value::<CreateJobRequest>(value).unwrap_err();
            assert!(
                error.to_string().contains("unknown field"),
                "unexpected error for {forbidden_field}: {error}",
            );
        }
    }

    #[test]
    fn job_has_a_stable_public_shape_and_round_trips() {
        let dto = JobDto {
            id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
            provider: ProviderDto::Prowlarr,
            result_ref: "prowlarr:result:7".to_owned(),
            state: JobStateDto::Queued,
            needs_action_reason: None,
            notify_scope: NotifyScopeDto::Family,
        };

        let value = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "provider": "prowlarr",
                "result_ref": "prowlarr:result:7",
                "state": "queued",
                "notify_scope": "family"
            }),
        );
        assert_eq!(serde_json::from_value::<JobDto>(value).unwrap(), dto);
    }

    #[test]
    fn queue_status_has_a_stable_public_shape_and_round_trips() {
        let status = QueueStatusDto {
            queued: 3,
            active: true,
        };
        let value = serde_json::to_value(status).unwrap();

        assert_eq!(value, serde_json::json!({ "queued": 3, "active": true }));
        assert_eq!(
            serde_json::from_value::<QueueStatusDto>(value).unwrap(),
            status,
        );
    }

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
