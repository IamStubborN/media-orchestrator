use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "idempotency_records")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub client_id: Uuid,
    pub idempotency_key: String,
    pub request_hash: Vec<u8>,
    pub status: String,
    pub response_status: Option<i16>,
    pub response_content_type: Option<String>,
    pub response_body: Option<Vec<u8>>,
    pub expires_at: TimeDateTimeWithTimeZone,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::api_client::Entity",
        from = "Column::ClientId",
        to = "super::api_client::Column::Id",
        on_delete = "Cascade"
    )]
    Client,
}

impl Related<super::api_client::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Client.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
