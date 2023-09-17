use std::{collections::HashMap, ops::Deref};

use crate::{persist::core::media::*, statics::DB};
use sea_orm::{entity::prelude::*, FromQueryResult, QueryOrder, QuerySelect};
use sea_query::{IntoCondition, JoinType};
use serde::{Deserialize, Serialize};

use super::{
    button, entity,
    messageentity::{self, DbMarkupType, EntityWithUser},
};
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize, Hash, Eq)]
#[sea_orm(table_name = "welcome")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub chat: i64,
    #[sea_orm(column_type = "Text")]
    pub text: Option<String>,
    pub media_id: Option<String>,
    pub media_type: Option<MediaType>,
    #[sea_orm(column_type = "Text")]
    pub goodbye_text: Option<String>,
    pub goodbye_media_id: Option<String>,
    pub goodbye_media_type: Option<MediaType>,
    #[sea_orm(default = false)]
    pub enabled: bool,
    pub entity_id: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::persist::core::entity::Entity",
        from = "Column::EntityId",
        to = "crate::persist::core::entity::Column::Id"
    )]
    Entities,
}

impl Related<crate::persist::core::entity::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Entities.def()
    }
}

impl Related<Entity> for crate::persist::core::entity::Entity {
    fn to() -> RelationDef {
        Relation::Entities.def().rev()
    }
}

impl ActiveModelBehavior for ActiveModel {}
#[derive(FromQueryResult)]
struct WelcomesWithEntities {
    /// Welcome fields    
    pub chat: Option<i64>,
    pub text: Option<String>,
    pub media_id: Option<String>,
    pub media_type: Option<MediaType>,
    pub goodbye_text: Option<String>,
    pub goodbye_media_id: Option<String>,
    pub goodbye_media_type: Option<MediaType>,
    pub enabled: Option<bool>,
    pub entity_id: Option<i64>,

    //button fields
    pub button_text: Option<String>,
    pub callback_data: Option<String>,
    pub button_url: Option<String>,
    pub owner_id: Option<i64>,
    pub pos_x: Option<u32>,
    pub pos_y: Option<u32>,

    // entity fields
    pub tg_type: Option<DbMarkupType>,
    pub offset: Option<i64>,
    pub length: Option<i64>,
    pub url: Option<String>,
    pub user: Option<i64>,
    pub language: Option<String>,
    pub emoji_id: Option<String>,

    // user fields
    pub user_id: Option<i64>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub is_bot: Option<bool>,
}

impl WelcomesWithEntities {
    fn get(self) -> (Option<Model>, Option<button::Model>, Option<EntityWithUser>) {
        let button = if let (Some(button_text), Some(owner_id), Some(pos_x), Some(pos_y)) =
            (self.button_text, self.owner_id, self.pos_x, self.pos_y)
        {
            Some(button::Model {
                button_text,
                owner_id,
                callback_data: self.callback_data,
                button_url: self.button_url,
                pos_x,
                pos_y,
            })
        } else {
            None
        };

        let filter = if let (Some(chat), Some(enabled)) = (self.chat, self.enabled) {
            Some(Model {
                chat,
                text: self.text,
                media_id: self.media_id,
                media_type: self.media_type,
                goodbye_text: self.goodbye_text,
                goodbye_media_id: self.goodbye_media_id,
                goodbye_media_type: self.goodbye_media_type,
                enabled,
                entity_id: self.entity_id,
            })
        } else {
            None
        };

        let entity = if let (Some(tg_type), Some(offset), Some(length), Some(owner_id)) =
            (self.tg_type, self.offset, self.length, self.owner_id)
        {
            Some(EntityWithUser {
                tg_type,
                offset,
                length,
                url: self.url,
                language: self.language,
                emoji_id: self.emoji_id,
                user: self.user,
                owner_id,
                user_id: self.user_id,
                first_name: self.first_name,
                last_name: self.last_name,
                username: self.username,
                is_bot: self.is_bot,
            })
        } else {
            None
        };

        (filter, button, entity)
    }
}

pub type FiltersMap = HashMap<Model, (Vec<EntityWithUser>, Vec<button::Model>)>;

pub async fn get_filters_join<F>(filter: F) -> crate::util::error::Result<FiltersMap>
where
    F: IntoCondition,
{
    let res = Entity::find()
        .select_only()
        .columns([
            Column::Chat,
            Column::Text,
            Column::MediaId,
            Column::MediaType,
            Column::GoodbyeText,
            Column::GoodbyeMediaId,
            Column::GoodbyeMediaType,
            Column::Enabled,
            Column::EntityId,
        ])
        .columns([
            messageentity::Column::TgType,
            messageentity::Column::Offset,
            messageentity::Column::Length,
            messageentity::Column::Url,
            messageentity::Column::User,
            messageentity::Column::Language,
            messageentity::Column::EmojiId,
            messageentity::Column::OwnerId,
        ])
        .columns([
            button::Column::ButtonText,
            button::Column::CallbackData,
            button::Column::ButtonUrl,
        ])
        .join(JoinType::LeftJoin, Relation::Entities.def())
        .join(JoinType::LeftJoin, entity::Relation::EntitiesRev.def())
        .join(JoinType::LeftJoin, entity::Relation::ButtonsRev.def())
        .join(JoinType::LeftJoin, messageentity::Relation::Users.def())
        .filter(filter)
        .order_by_asc(button::Column::PosX)
        .order_by_asc(button::Column::PosY)
        .into_model::<WelcomesWithEntities>()
        .all(DB.deref())
        .await?;

    let res = res.into_iter().map(|v| v.get()).fold(
        FiltersMap::new(),
        |mut acc, (filter, button, entity)| {
            if let Some(filter) = filter {
                let (entitylist, buttonlist) = acc
                    .entry(filter)
                    .or_insert_with(|| (Vec::new(), Vec::new()));

                if let Some(button) = button {
                    buttonlist.push(button);
                }

                if let Some(entity) = entity {
                    entitylist.push(entity);
                }
            }
            acc
        },
    );

    Ok(res)
}
