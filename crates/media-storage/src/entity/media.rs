use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "media")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub release_year: Option<i32>,
    pub series_ordering: Option<String>,
    pub metadata_snapshot: Json,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::media_external_ref::Entity")]
    ExternalReference,
    #[sea_orm(has_many = "super::season::Entity")]
    Season,
}

impl Related<super::media_external_ref::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExternalReference.def()
    }
}

impl Related<super::season::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Season.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
