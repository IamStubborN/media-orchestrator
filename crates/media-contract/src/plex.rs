#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlexReconcileRequest {
    pub path: String,
    pub canonical_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlexReconcileStatus {
    Matched,
    Pending,
    Mismatch,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlexObservationDto {
    pub path: String,
    pub canonical_id: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlexReconcileResponse {
    pub status: PlexReconcileStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<PlexObservationDto>,
}
