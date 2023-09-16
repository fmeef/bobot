//! ORM type for storing user information. Since redis is used for this ephemerally
//! in most cases this is very simple

use botapi::gen_types::{User, UserBuilder};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub user_id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub is_bot: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl Into<User> for Model {
    fn into(self) -> User {
        let mut builder = UserBuilder::new(self.user_id, self.is_bot, self.first_name);
        if let Some(name) = self.last_name {
            builder = builder.set_last_name(name);
        }
        builder.build()
    }
}

impl Model {
    pub fn from_user(value: &User) -> Self {
        Self {
            user_id: value.get_id(),
            first_name: value.get_first_name().into_owned(),
            last_name: value.get_last_name().map(|v| v.into_owned()),
            username: value.get_username().map(|v| v.into_owned()),
            is_bot: value.get_is_bot(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
