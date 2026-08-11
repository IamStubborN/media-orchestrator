use crate::{ExecutionSelectionDto, JobDto, PublicId};

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LeaseDto {
    pub lease_id: PublicId,
    pub job: JobDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionSelectionDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_task_ordinals: Vec<u32>,
    pub expires_at: String,
}

#[cfg(test)]
mod tests {
    use super::LeaseDto;
    use crate::{JobDto, JobStateDto, NeedsActionReasonDto, NotifyScopeDto, ProviderDto, PublicId};

    #[test]
    fn lease_has_a_stable_public_shape_and_round_trips() {
        let lease = LeaseDto {
            lease_id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb112").unwrap(),
            job: JobDto {
                id: PublicId::parse("018f3f86-7b4c-7b4f-9b6a-6d62f45bb111").unwrap(),
                provider: ProviderDto::Rezka,
                result_ref: "rezka:series:42:season:1".to_owned(),
                state: JobStateDto::NeedsAction,
                needs_action_reason: Some(NeedsActionReasonDto::IdentityAmbiguous),
                notify_scope: NotifyScopeDto::Initiator,
                lifecycle_cycle: 1,
            },
            execution: None,
            completed_task_ordinals: vec![0, 1],
            expires_at: "2026-07-10T18:01:00Z".to_owned(),
        };

        let value = serde_json::to_value(&lease).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "lease_id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb112",
                "job": {
                    "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                    "provider": "rezka",
                    "result_ref": "rezka:series:42:season:1",
                    "state": "needs_action",
                    "needs_action_reason": "identity_ambiguous",
                    "notify_scope": "initiator",
                    "lifecycle_cycle": 1
                },
                "completed_task_ordinals": [0, 1],
                "expires_at": "2026-07-10T18:01:00Z"
            }),
        );
        assert_eq!(serde_json::from_value::<LeaseDto>(value).unwrap(), lease);
    }

    #[test]
    fn lease_contract_has_no_client_controlled_ttl() {
        let value = serde_json::json!({
            "lease_id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb112",
            "job": {
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "provider": "prowlarr",
                "result_ref": "prowlarr:result:7",
                "state": "leased",
                "notify_scope": "family"
            },
            "expires_at": "2026-07-10T18:01:00Z"
        });

        let lease = serde_json::from_value::<LeaseDto>(value).unwrap();
        let encoded = serde_json::to_value(lease).unwrap();
        assert_eq!(encoded.get("ttl"), None);
        assert_eq!(encoded.get("heartbeat_ttl"), None);
        assert_eq!(encoded.get("expires_in"), None);
    }
}
