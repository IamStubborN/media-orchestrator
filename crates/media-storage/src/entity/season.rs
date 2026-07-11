use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "seasons")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub media_id: Uuid,
    pub season_number: i32,
    pub title: Option<String>,
    pub metadata_snapshot: Json,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::media::Entity",
        from = "Column::MediaId",
        to = "super::media::Column::Id",
        on_delete = "Cascade"
    )]
    Media,
    #[sea_orm(has_many = "super::episode::Entity")]
    Episode,
}

impl Related<super::media::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Media.def()
    }
}

impl Related<super::episode::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Episode.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
