#[derive(Debug, Copy, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerLifecycleStateDto {
    Ready,
    Rotating,
    Blocked,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRunnerLifecycleRequest {
    pub state: RunnerLifecycleStateDto,
    pub reason: Option<String>,
    pub previous_ip: Option<String>,
    pub current_ip: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunnerLifecycleDto {
    pub state: RunnerLifecycleStateDto,
    pub reason: Option<String>,
    pub previous_ip: Option<String>,
    pub current_ip: Option<String>,
    pub updated_at: String,
}
