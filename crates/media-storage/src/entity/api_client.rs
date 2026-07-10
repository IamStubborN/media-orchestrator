use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "api_clients")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub user_id: Option<Uuid>,
    pub credential_digest: Vec<u8>,
    pub enabled: bool,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiClientModel")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("user_id", &self.user_id)
            .field("credential_digest", &"[REDACTED]")
            .field("enabled", &self.enabled)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Restrict"
    )]
    User,
    #[sea_orm(has_many = "super::idempotency_record::Entity")]
    IdempotencyRecord,
    #[sea_orm(has_many = "super::job_lease::Entity")]
    JobLease,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::idempotency_record::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::IdempotencyRecord.def()
    }
}

impl Related<super::job_lease::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::JobLease.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::Model;

    #[test]
    fn model_debug_redacts_the_credential_digest() {
        let model = Model {
            id: uuid::Uuid::nil(),
            name: "client".to_owned(),
            role: "runner".to_owned(),
            user_id: None,
            credential_digest: vec![0x2a; 32],
            enabled: true,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        };

        let debug = format!("{model:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("42"));
        assert!(!debug.contains("2a"));
    }
}
