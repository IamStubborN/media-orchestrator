use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "operation_receipts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub operation_key: Vec<u8>,
    pub operation_kind: String,
    pub result_kind: String,
    pub result_snapshot: Option<Json>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Model")
            .field("id", &self.id)
            .field("operation_key", &"[REDACTED]")
            .field("operation_kind", &self.operation_kind)
            .field("result_kind", &self.result_kind)
            .field("result_snapshot", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::Model;

    #[test]
    fn debug_output_redacts_operation_key_and_result_snapshot() {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        let model = Model {
            id: uuid::Uuid::nil(),
            operation_key: vec![222, 173, 190, 239],
            operation_kind: "create_job".to_owned(),
            result_kind: "job".to_owned(),
            result_snapshot: Some(serde_json::json!({"private": "secret-result-value"})),
            created_at: now,
            updated_at: now,
        };

        let debug = format!("{model:?}");
        assert!(!debug.contains("222, 173, 190, 239"));
        assert!(!debug.contains("secret-result-value"));
        assert!(debug.contains("operation_key: \"[REDACTED]\""));
        assert!(debug.contains("result_snapshot: \"[REDACTED]\""));
    }
}
