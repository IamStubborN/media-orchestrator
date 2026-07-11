use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "job_leases")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub slot: i16,
    pub job_id: Uuid,
    pub runner_client_id: Uuid,
    pub expires_at: TimeDateTimeWithTimeZone,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::job::Entity",
        from = "Column::JobId",
        to = "super::job::Column::Id",
        on_delete = "Restrict"
    )]
    Job,
    #[sea_orm(
        belongs_to = "super::api_client::Entity",
        from = "Column::RunnerClientId",
        to = "super::api_client::Column::Id",
        on_delete = "Restrict"
    )]
    RunnerClient,
}

impl Related<super::job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Job.def()
    }
}

impl Related<super::api_client::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RunnerClient.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
