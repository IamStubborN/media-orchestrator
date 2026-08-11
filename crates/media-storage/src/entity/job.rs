use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "jobs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub owner_id: Uuid,
    pub provider: String,
    pub result_ref: String,
    pub state: String,
    pub needs_action_reason: Option<String>,
    pub notify_scope: String,
    pub notification_cycle: i64,
    pub request_snapshot: Json,
    pub error_snapshot: Option<Json>,
    pub attempt_count: i32,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
    pub started_at: Option<TimeDateTimeWithTimeZone>,
    pub completed_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnerId",
        to = "super::user::Column::Id",
        on_delete = "Restrict"
    )]
    Owner,
    #[sea_orm(has_many = "super::job_task::Entity")]
    Task,
    #[sea_orm(has_many = "super::job_lease::Entity")]
    Lease,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Owner.def()
    }
}

impl Related<super::job_task::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Task.def()
    }
}

impl Related<super::job_lease::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Lease.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
