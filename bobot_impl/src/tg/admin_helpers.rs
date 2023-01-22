use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
};

use crate::{
    persist::{
        admin::actions,
        redis::{default_cache_query, CachedQueryTrait, RedisStr},
    },
    statics::{DB, REDIS, TG},
    util::error::{BotError, Result},
    util::string::{get_chat_lang, Speak},
};
use async_trait::async_trait;
use botapi::gen_types::{Chat, ChatMember, ChatPermissions, Message, User};
use chrono::Duration;
use futures::{future::BoxFuture, FutureExt};
use lazy_static::__Deref;
use macros::rlformat;
use redis::AsyncCommands;
use sea_orm::{sea_query::OnConflict, EntityTrait, IntoActiveModel};

use super::{
    command::EntityArg,
    user::{get_me, get_user_username, GetUser},
};

pub async fn is_self_admin(chat: &Chat) -> Result<bool> {
    let me = get_me().await?;
    Ok(chat.is_user_admin(me.get_id()).await?.is_some())
}

pub fn is_dm(chat: &Chat) -> bool {
    chat.get_tg_type() == "private"
}

fn get_action_key(user: i64, chat: i64) -> String {
    format!("act:{}:{}", user, chat)
}

pub async fn change_permissions(
    chat: &Chat,
    user: &User,
    permissions: &ChatPermissions,
) -> Result<()> {
    let me = get_me().await?;
    let lang = get_chat_lang(chat.get_id()).await?;
    if user.is_admin(chat).await? {
        Err(BotError::speak(rlformat!(lang, "muteadmin"), chat.get_id()))
    } else {
        if user.get_id() == me.get_id() {
            chat.speak(rlformat!(lang, "mutemyself")).await?;
            Err(BotError::speak(
                rlformat!(lang, "mutemyself"),
                chat.get_id(),
            ))
        } else {
            TG.client()
                .build_restrict_chat_member(chat.get_id(), user.get_id(), permissions)
                .build()
                .await?;
            Ok(())
        }
    }
}

pub async fn action_message<'a, F>(
    message: &'a Message,
    entities: &VecDeque<EntityArg<'a>>,
    action: F,
) -> Result<()>
where
    for<'b> F: FnOnce(&'b Chat, &'b User) -> BoxFuture<'b, Result<()>>,
{
    is_group_or_die(&message.get_chat()).await?;
    self_admin_or_die(&message.get_chat()).await?;
    message.get_from().admin_or_die(&message.get_chat()).await?;
    let lang = get_chat_lang(message.get_chat().get_id()).await?;

    if let Some(user) = message
        .get_reply_to_message_ref()
        .map(|v| v.get_from())
        .flatten()
    {
        action(&message.get_chat_ref(), &user).await?;
    } else {
        match entities.front() {
            Some(EntityArg::Mention(name)) => {
                if let Some(user) = get_user_username(name).await? {
                    action(message.get_chat_ref(), &user).await?;
                } else {
                    return Err(BotError::speak(
                        rlformat!(lang, "usernotfound"),
                        message.get_chat().get_id(),
                    ));
                }
            }
            Some(EntityArg::TextMention(user)) => {
                action(message.get_chat_ref(), user).await?;
            }
            _ => {
                return Err(BotError::speak(
                    rlformat!(lang, "specifyuser"),
                    message.get_chat().get_id(),
                ));
            }
        };
    }
    Ok(())
}

pub async fn change_permissions_message<'a>(
    message: &Message,
    entities: &VecDeque<EntityArg<'a>>,
    permissions: ChatPermissions,
) -> Result<()> {
    action_message(message, entities, |chat, user| {
        async move { change_permissions(chat, user, &permissions).await }.boxed()
    })
    .await?;
    Ok(())
}

pub async fn get_actions(chat: &Chat, user: &User) -> Result<Option<actions::Model>> {
    let chat = chat.get_id();
    let user = user.get_id();
    let key = get_action_key(user, chat);
    let res = default_cache_query(
        move |_, _| async move {
            let res = actions::Entity::find_by_id((user, chat))
                .one(DB.deref())
                .await?;
            Ok(res)
        },
        Duration::hours(1),
    )
    .query(&key, &())
    .await?;
    Ok(res)
}

pub async fn update_actions(actions: actions::Model) -> Result<()> {
    let r = RedisStr::new(&actions)?;
    let key = get_action_key(actions.user_id, actions.chat_id);
    REDIS
        .pipe(|p| {
            p.set(&key, r)
                .expire(&key, Duration::hours(1).num_seconds() as usize)
        })
        .await?;

    actions::Entity::insert(actions.into_active_model())
        .on_conflict(
            OnConflict::columns([actions::Column::UserId, actions::Column::ChatId])
                .update_columns([
                    actions::Column::Warns,
                    actions::Column::IsBanned,
                    actions::Column::IsMuted,
                    actions::Column::Action,
                ])
                .to_owned(),
        )
        .exec(DB.deref().deref())
        .await?;
    Ok(())
}

pub async fn is_dm_or_die(chat: &Chat) -> Result<()> {
    let lang = get_chat_lang(chat.get_id()).await?;
    if !is_dm(chat) {
        Err(BotError::speak(rlformat!(lang, "notdm"), chat.get_id()))
    } else {
        Ok(())
    }
}

pub async fn is_group_or_die(chat: &Chat) -> Result<()> {
    let lang = get_chat_lang(chat.get_id()).await?;
    match chat.get_tg_type().as_ref() {
        "private" => Err(BotError::speak(rlformat!(lang, "baddm"), chat.get_id())),
        "group" => Err(BotError::speak(
            rlformat!(lang, "notsupergroup"),
            chat.get_id(),
        )),
        _ => Ok(()),
    }
}

pub async fn self_admin_or_die(chat: &Chat) -> Result<()> {
    if !is_self_admin(chat).await? {
        let lang = get_chat_lang(chat.get_id()).await?;
        Err(BotError::speak(
            rlformat!(lang, "needtobeadmin"),
            chat.get_id(),
        ))
    } else {
        Ok(())
    }
}

fn get_chat_admin_cache_key(chat: i64) -> String {
    format!("ca:{}", chat)
}

#[async_trait]
pub trait IsAdmin {
    async fn is_admin(&self, chat: &Chat) -> Result<bool>;
    async fn admin_or_die(&self, chat: &Chat) -> Result<()>;
}

#[async_trait]
pub trait GetCachedAdmins {
    async fn get_cached_admins(&self) -> Result<HashMap<i64, ChatMember>>;
    async fn refresh_cached_admins(&self) -> Result<HashMap<i64, ChatMember>>;
    async fn is_user_admin(&self, user: i64) -> Result<Option<ChatMember>>;
}

#[async_trait]
impl IsAdmin for User {
    async fn is_admin(&self, chat: &Chat) -> Result<bool> {
        Ok(chat.is_user_admin(self.get_id()).await?.is_some())
    }

    async fn admin_or_die(&self, chat: &Chat) -> Result<()> {
        if self.is_admin(chat).await? {
            Ok(())
        } else {
            let lang = get_chat_lang(chat.get_id()).await?;
            let msg = rlformat!(
                lang,
                "lackingadminrights",
                self.get_username_ref()
                    .unwrap_or(self.get_id().to_string().as_str())
            );
            Err(BotError::speak(msg, chat.get_id()))
        }
    }
}

#[async_trait]
impl<'a> IsAdmin for Option<Cow<'a, User>> {
    async fn is_admin(&self, chat: &Chat) -> Result<bool> {
        if let Some(user) = self {
            Ok(chat.is_user_admin(user.get_id()).await?.is_some())
        } else {
            Ok(false)
        }
    }

    async fn admin_or_die(&self, chat: &Chat) -> Result<()> {
        if let Some(user) = self {
            if user.is_admin(chat).await? {
                Ok(())
            } else {
                let lang = get_chat_lang(chat.get_id()).await?;
                let msg = rlformat!(
                    lang,
                    "lackingadminrights",
                    user.get_username_ref()
                        .unwrap_or(user.get_id().to_string().as_str())
                );
                Err(BotError::speak(msg, chat.get_id()))
            }
        } else {
            Err(BotError::Generic("fail".to_owned()))
        }
    }
}

#[async_trait]
impl IsAdmin for i64 {
    async fn is_admin(&self, chat: &Chat) -> Result<bool> {
        Ok(chat.is_user_admin(*self).await?.is_some())
    }

    async fn admin_or_die(&self, chat: &Chat) -> Result<()> {
        if self.is_admin(chat).await? {
            Ok(())
        } else {
            let lang = get_chat_lang(chat.get_id()).await?;
            let msg = if let Some(user) = self.get_cached_user().await? {
                rlformat!(
                    lang,
                    "lackingadminrights",
                    user.get_username_ref().unwrap_or(self.to_string().as_str())
                )
            } else {
                rlformat!(lang, "lackingadminrights", self)
            };

            Err(BotError::speak(msg, chat.get_id()))
        }
    }
}

#[async_trait]
impl GetCachedAdmins for Chat {
    async fn get_cached_admins(&self) -> Result<HashMap<i64, ChatMember>> {
        let key = get_chat_admin_cache_key(self.get_id());
        let admins: Option<HashMap<i64, RedisStr>> = REDIS.sq(|q| q.hgetall(&key)).await?;
        if let Some(admins) = admins {
            let admins = admins
                .into_iter()
                .map(|(k, v)| (k, v.get::<ChatMember>()))
                .try_fold(HashMap::new(), |mut acc, (k, v)| {
                    acc.insert(k, v?);
                    Ok::<_, BotError>(acc)
                })?;
            Ok(admins)
        } else {
            self.refresh_cached_admins().await
        }
    }

    async fn is_user_admin(&self, user: i64) -> Result<Option<ChatMember>> {
        let key = get_chat_admin_cache_key(self.get_id());
        let admin: Option<RedisStr> = REDIS.sq(|q| q.hget(&key, user)).await?;
        if let Some(user) = admin {
            Ok(Some(user.get::<ChatMember>()?))
        } else {
            Ok(None)
        }
    }

    async fn refresh_cached_admins(&self) -> Result<HashMap<i64, ChatMember>> {
        let admins = TG
            .client()
            .build_get_chat_administrators(self.get_id())
            .chat_id(self.get_id())
            .build()
            .await?;
        let res = admins
            .iter()
            .cloned()
            .map(|cm| (cm.get_user().get_id(), cm))
            .collect::<HashMap<i64, ChatMember>>();
        let mut admins = admins.into_iter().map(|cm| (cm.get_user().get_id(), cm));
        let lockkey = format!("aclock:{}", self.get_id());
        if !REDIS.sq(|q| q.exists(&lockkey)).await? {
            let key = get_chat_admin_cache_key(self.get_id());

            REDIS
                .try_pipe(|q| {
                    q.set(&lockkey, true);
                    q.expire(&lockkey, Duration::minutes(10).num_seconds() as usize);
                    admins.try_for_each(|(id, cm)| {
                        q.hset(&key, id, RedisStr::new(&cm)?);
                        Ok::<(), BotError>(())
                    })?;
                    Ok(q.expire(&key, Duration::hours(48).num_seconds() as usize))
                })
                .await?;
            Ok(res)
        } else {
            let lang = get_chat_lang(self.get_id()).await?;
            Err(BotError::speak(rlformat!(lang, "cachewait"), self.get_id()))
        }
    }
}
