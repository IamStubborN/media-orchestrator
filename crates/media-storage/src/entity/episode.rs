use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "episodes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub season_id: Uuid,
    pub episode_number: i32,
    pub absolute_number: Option<i32>,
    pub title: Option<String>,
    pub metadata_snapshot: Json,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::season::Entity",
        from = "Column::SeasonId",
        to = "super::season::Column::Id",
        on_delete = "Cascade"
    )]
    Season,
    #[sea_orm(has_many = "super::episode_provider_mapping::Entity")]
    ProviderMapping,
    #[sea_orm(has_many = "super::job_task::Entity")]
    JobTask,
}

impl Related<super::season::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Season.def()
    }
}

impl Related<super::episode_provider_mapping::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ProviderMapping.def()
    }
}

impl Related<super::job_task::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::JobTask.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
