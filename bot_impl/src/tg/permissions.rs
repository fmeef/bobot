//! Admin permissions management interface. Allows for both admin/notadmin permissions and
//! more granular permissions based on telegram's own system

use std::{borrow::Cow, collections::HashMap};

use crate::{
    persist::redis::RedisStr,
    statics::{CONFIG, REDIS, TG},
    util::error::{BotError, Result},
    util::string::get_chat_lang,
};
use async_trait::async_trait;
use botapi::gen_types::{Chat, ChatMember, ChatMemberAdministrator, Message, UpdateExt, User};
use chrono::Duration;

use itertools::Itertools;
use macros::lang_fmt;
use redis::AsyncCommands;

use super::{
    admin_helpers::{is_group_or_die, is_self_admin},
    user::{GetUser, Username},
};

/// Granular permissions with associated humanreadable name. Used for printing
/// detailed error messages to users
#[derive(Clone, Debug)]
pub struct NamedBotPermissions {
    pub can_manage_chat: NamedPermission,
    pub can_restrict_members: NamedPermission,
    pub can_delete_messages: NamedPermission,
    pub can_change_info: NamedPermission,
    pub can_promote_members: NamedPermission,
    pub can_pin_messages: NamedPermission,
    pub is_sudo: NamedPermission,
    pub is_support: NamedPermission,
}

impl NamedBotPermissions {
    /// Use the admin cache to check a user's permissions in a group
    pub async fn from_chatuser(user: &User, chat: &Chat) -> Result<Self> {
        let mut v = if let Some(admin) = chat.is_user_admin(user.get_id()).await? {
            Ok::<Self, BotError>(admin.into())
        } else {
            let v: NamedBotPermissions = BotPermissions {
                can_manage_chat: false,
                can_restrict_members: false,
                can_delete_messages: false,
                can_change_info: false,
                can_promote_members: false,
                can_pin_messages: false,
            }
            .into();
            Ok(v)
        }?;

        if CONFIG.admin.sudo_users.contains(&user.get_id()) {
            v.is_sudo.0.iter_mut().for_each(|v| v.val = true);
            v.is_support.0.iter_mut().for_each(|v| v.val = true);
        }

        if CONFIG.admin.support_users.contains(&user.get_id()) {
            v.is_support.0.iter_mut().for_each(|v| v.val = true);
        }

        Ok(v)
    }

    /// Check the permissions of a message's sender. Returns an error if the message has
    /// no sender
    pub async fn from_message(message: &Message) -> Result<Self> {
        let chat = message.get_chat();
        let user = message.get_from().ok_or_else(|| {
            BotError::speak("Permission denied, user does not exist", chat.get_id())
        })?;
        Self::from_chatuser(&user, &chat).await
    }
}

impl From<ChatMemberAdministrator> for NamedBotPermissions {
    fn from(value: ChatMemberAdministrator) -> Self {
        BotPermissions {
            can_manage_chat: value.get_can_manage_chat(),
            can_restrict_members: value.get_can_restrict_members(),
            can_delete_messages: value.get_can_delete_messages(),
            can_change_info: value.get_can_change_info(),
            can_promote_members: value.get_can_promote_members(),
            can_pin_messages: value.get_can_pin_messages().unwrap_or(false),
        }
        .into()
    }
}

impl From<ChatMember> for NamedBotPermissions {
    fn from(value: ChatMember) -> Self {
        match value {
            ChatMember::ChatMemberAdministrator(admin) => NamedBotPermissions::from(admin),
            ChatMember::ChatMemberOwner(_) => BotPermissions {
                can_manage_chat: true,
                can_restrict_members: true,
                can_delete_messages: true,
                can_change_info: true,
                can_promote_members: true,
                can_pin_messages: true,
            }
            .into(),
            _ => BotPermissions {
                can_manage_chat: false,
                can_restrict_members: false,
                can_delete_messages: false,
                can_change_info: false,
                can_promote_members: false,
                can_pin_messages: false,
            }
            .into(),
        }
    }
}

/// A single permission with granted value and human readable name
#[derive(Clone, Debug)]
pub struct NamedPermissionContent {
    name: &'static str,
    val: bool,
}

#[derive(Clone, Debug)]
pub struct NamedPermission(Vec<NamedPermissionContent>);

impl Into<bool> for NamedPermission {
    fn into(self) -> bool {
        self.0.iter().all(|v| v.val)
    }
}

impl NamedPermission {
    /// Construct a new NamedPermission with name
    fn new(name: &'static str, val: bool) -> Self {
        Self(vec![NamedPermissionContent::new(name, val)])
    }

    /// Returns true if all permissions are granted
    fn is_granted(&self) -> bool {
        self.0.iter().all(|v| v.val)
    }

    /// Gets the human readable name, including combined permissions
    fn get_name(&self) -> String {
        self.0.iter().map(|v| v.name).join(" and ")
    }

    /// Combine two permissions to make a compound permisssion
    pub fn and(mut self, new_perm: Self) -> Self {
        self.0.extend(new_perm.0);
        self
    }
}

impl NamedPermissionContent {
    /// Construct a new NamedPermission with name
    fn new(name: &'static str, val: bool) -> Self {
        Self { name, val }
    }
}

/// Permissions for chat admins (and the bot itself). Based off of telegram API's
/// ChatPermissions
#[derive(Clone, Debug)]
pub struct BotPermissions {
    pub can_manage_chat: bool,
    pub can_restrict_members: bool,
    pub can_delete_messages: bool,
    pub can_change_info: bool,
    pub can_promote_members: bool,
    pub can_pin_messages: bool,
}

impl Into<NamedBotPermissions> for BotPermissions {
    fn into(self) -> NamedBotPermissions {
        NamedBotPermissions {
            can_manage_chat: NamedPermission::new("CanManageChat", self.can_manage_chat),
            can_restrict_members: NamedPermission::new(
                "CanRestrictMembers",
                self.can_restrict_members,
            ),
            can_delete_messages: NamedPermission::new(
                "CanDeleteMessasges",
                self.can_delete_messages,
            ),
            can_change_info: NamedPermission::new("CanChangeInfo", self.can_change_info),
            can_promote_members: NamedPermission::new(
                "CanPromoteMembers",
                self.can_promote_members,
            ),
            can_pin_messages: NamedPermission::new("CanPinMessages", self.can_pin_messages),
            is_sudo: NamedPermission::new("Sudo", false),
            is_support: NamedPermission::new("Support", false),
        }
    }
}

impl From<NamedBotPermissions> for BotPermissions {
    fn from(value: NamedBotPermissions) -> Self {
        Self {
            can_manage_chat: value.can_manage_chat.is_granted(),
            can_restrict_members: value.can_restrict_members.is_granted(),
            can_delete_messages: value.can_delete_messages.is_granted(),
            can_change_info: value.can_change_info.is_granted(),
            can_promote_members: value.can_promote_members.is_granted(),
            can_pin_messages: value.can_pin_messages.is_granted(),
        }
    }
}

/// Extension trait for determining if users or user-like objects are admin
#[async_trait]
pub trait IsAdmin {
    /// Returns true if the user is an admin
    async fn is_admin(&self, chat: &Chat) -> Result<bool>;

    /// If the user is not an admin fail with printable error
    async fn admin_or_die(&self, chat: &Chat) -> Result<()>;

    /// Get the permissions for a user
    async fn get_permissions(&self, chat: &Chat) -> Result<BotPermissions>;

    /// Apply the mapper function to the permissions, if it returns false NamedPermissions,
    /// return with error
    async fn check_permissions<F>(&self, chat: &Chat, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send;
}

/// Extension trait similar to IsAdmin, but with checking for supergroups.
/// Assumes that the implementor has a chat context
#[async_trait]
pub trait IsGroupAdmin {
    /// If the user is not admin or the group is not a supergroup, return a printable error
    async fn legacy_check_permissions(&self) -> Result<()>;

    /// return true if the group is a supergroup and the user is an admin
    async fn is_group_admin(&self) -> Result<bool>;

    /// get the permissions for a user
    async fn get_permissions(&self) -> Result<BotPermissions>;

    /// Apply the mapper function to the permissions, if it returns false NamedPermissions,
    /// return with error
    async fn check_permissions<F>(&self, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send;
}

/// Defines behavior for interacting with the admin cache. Implementors should have
/// somne concept of a User and Chat
#[async_trait]
pub trait GetCachedAdmins {
    /// Retreive the entire admin cache, refreshing as needed
    async fn get_cached_admins(&self) -> Result<HashMap<i64, ChatMember>>;

    /// Manually refresh admin cache. This is ratelimited to 10 minutes between requests
    async fn refresh_cached_admins(&self) -> Result<HashMap<i64, ChatMember>>;

    /// Search the admin cache for a member
    async fn is_user_admin(&self, user: i64) -> Result<Option<ChatMember>>;

    /// Promote a user, caching the promotion without refreshing
    async fn promote(&self, user: i64) -> Result<()>;

    /// Demotes a user, caching the demotion without refreshing
    async fn demote(&self, user: i64) -> Result<()>;
}

#[async_trait]
impl IsGroupAdmin for Message {
    async fn is_group_admin(&self) -> Result<bool> {
        if let Some(user) = self.get_from() {
            user.is_admin(self.get_chat_ref()).await
        } else {
            Ok(false)
        }
    }

    async fn legacy_check_permissions(&self) -> Result<()> {
        is_group_or_die(self.get_chat_ref()).await?;
        self_admin_or_die(self.get_chat_ref()).await?;

        if self.is_group_admin().await? {
            Ok(())
        } else if let Some(user) = self.get_from() {
            let lang = get_chat_lang(self.get_chat().get_id()).await?;
            let msg = lang_fmt!(lang, "lackingadminrights", user.name_humanreadable());
            Err(BotError::speak(msg, self.get_chat().get_id()))
        } else {
            Err(BotError::Generic("not admin".to_owned()))
        }
    }

    async fn get_permissions(&self) -> Result<BotPermissions> {
        let user = self
            .get_from()
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;

        let chat = self.get_chat_ref();
        let res = NamedBotPermissions::from_chatuser(&user, chat).await?;
        Ok(res.into())
    }
    async fn check_permissions<F>(&self, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send,
    {
        let chat = self.get_chat_ref();
        is_group_or_die(&chat).await?;

        let user = self
            .get_from()
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;
        let permission = NamedBotPermissions::from_chatuser(&user, chat).await?;
        // log::info!("got permissions {:?}", permission);

        let sudo = permission.is_sudo.is_granted();
        let p = func(permission);

        if !p.is_granted() && !sudo {
            Err(BotError::speak(
                format!("Permission denied. User missing \"{}\"", p.get_name()),
                chat.get_id(),
            ))
        } else {
            Ok(())
        }
    }
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
            let msg = lang_fmt!(lang, "lackingadminrights", self.name_humanreadable());
            Err(BotError::speak(msg, chat.get_id()))
        }
    }

    async fn get_permissions(&self, chat: &Chat) -> Result<BotPermissions> {
        let res = NamedBotPermissions::from_chatuser(self, chat).await?;
        Ok(res.into())
    }
    async fn check_permissions<F>(&self, chat: &Chat, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send,
    {
        is_group_or_die(chat).await?;
        let permission = NamedBotPermissions::from_chatuser(self, chat).await?;
        let sudo = permission.is_sudo.is_granted();
        let p = func(permission);
        if !p.is_granted() && !sudo {
            Err(BotError::speak(
                format!("Permission denied. User missing \"{}\"", p.get_name()),
                chat.get_id(),
            ))
        } else {
            Ok(())
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
                let msg = lang_fmt!(
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

    async fn get_permissions(&self, chat: &Chat) -> Result<BotPermissions> {
        let user = self
            .as_ref()
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;
        let res = NamedBotPermissions::from_chatuser(&user, chat).await?;
        Ok(res.into())
    }
    async fn check_permissions<F>(&self, chat: &Chat, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send,
    {
        is_group_or_die(chat).await?;
        let user = self
            .as_ref()
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;
        let permission = NamedBotPermissions::from_chatuser(&user, chat).await?;
        let sudo = permission.is_sudo.is_granted();
        let p = func(permission);
        if !p.is_granted() && !sudo {
            Err(BotError::speak(
                format!("Permission denied. User missing \"{}\"", p.get_name()),
                chat.get_id(),
            ))
        } else {
            Ok(())
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
                lang_fmt!(
                    lang,
                    "lackingadminrights",
                    user.get_username_ref().unwrap_or(self.to_string().as_str())
                )
            } else {
                lang_fmt!(lang, "lackingadminrights", self)
            };

            Err(BotError::speak(msg, chat.get_id()))
        }
    }

    async fn get_permissions(&self, chat: &Chat) -> Result<BotPermissions> {
        let user = self
            .get_cached_user()
            .await?
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;
        let res = NamedBotPermissions::from_chatuser(&user, chat).await?;
        Ok(res.into())
    }
    async fn check_permissions<F>(&self, chat: &Chat, func: F) -> Result<()>
    where
        F: FnOnce(NamedBotPermissions) -> NamedPermission + Send,
    {
        is_group_or_die(chat).await?;
        let user = self
            .get_cached_user()
            .await?
            .ok_or_else(|| BotError::Generic("user not found".to_owned()))?;
        let permission = NamedBotPermissions::from_chatuser(&user, chat).await?;
        let sudo = permission.is_sudo.is_granted();
        let p = func(permission);
        if !p.is_granted() && !sudo {
            Err(BotError::speak(
                format!("Permission denied. User missing \"{}\"", p.get_name()),
                chat.get_id(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Updates the admin cache with any changes in the bot's admin status
pub async fn update_self_admin(update: &UpdateExt) -> Result<()> {
    if let UpdateExt::MyChatMember(member) = update {
        let key = get_chat_admin_cache_key(member.get_chat().get_id());
        match member.get_new_chat_member_ref() {
            ChatMember::ChatMemberAdministrator(ref admin) => {
                log::info!("bot updated to admin");
                let user_id = admin.get_user().get_id();
                let admin = ChatMember::ChatMemberAdministrator(admin.to_owned());
                let admin = RedisStr::new(&admin)?;
                REDIS.sq(|q| q.hset(&key, user_id, admin)).await?;
            }
            ChatMember::ChatMemberOwner(ref owner) => {
                log::info!("Im soemhow the owner. What?");
                let user_id = owner.get_user().get_id();
                let admin = ChatMember::ChatMemberOwner(owner.to_owned());
                let admin = RedisStr::new(&admin)?;
                REDIS.sq(|q| q.hset(&key, user_id, admin)).await?;
            }
            mamber => {
                log::info!("Im not admin anymore ;(");
                let user_id = mamber.get_user().get_id();
                REDIS.sq(|q| q.hdel(&key, user_id)).await?;
            }
        }
    }

    Ok(())
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
        let (exists, ke, admin): (bool, bool, Option<RedisStr>) = REDIS
            .pipe(|q| q.atomic().exists(&key).hexists(&key, user).hget(&key, user))
            .await?;
        if exists {
            if !ke {
                return Ok(None);
            }
            if let Some(user) = admin {
                Ok(Some(user.get::<ChatMember>()?))
            } else {
                Ok(None)
            }
        } else {
            Ok(self.refresh_cached_admins().await?.remove(&user))
        }
    }

    async fn promote(&self, user: i64) -> Result<()> {
        TG.client()
            .build_promote_chat_member(self.get_id(), user)
            .can_manage_chat(true)
            .can_restrict_members(true)
            .can_post_messages(true)
            .can_edit_messages(true)
            .can_manage_video_chats(true)
            .can_change_info(true)
            .can_invite_users(true)
            .can_pin_messages(true)
            .can_delete_messages(true)
            .can_promote_members(true)
            .build()
            .await?;

        let mamber = TG
            .client()
            .build_get_chat_member(self.get_id(), user)
            .build()
            .await?;
        let key = get_chat_admin_cache_key(self.get_id());
        let cm = RedisStr::new(&mamber)?;
        REDIS.sq(|q| q.hset(&key, user, cm)).await?;
        Ok(())
    }

    async fn demote(&self, user: i64) -> Result<()> {
        TG.client()
            .build_promote_chat_member(self.get_id(), user)
            .can_manage_chat(false)
            .can_restrict_members(false)
            .can_post_messages(false)
            .can_edit_messages(false)
            .can_manage_video_chats(false)
            .can_change_info(false)
            .can_invite_users(false)
            .can_pin_messages(false)
            .can_delete_messages(false)
            .can_promote_members(false)
            .build()
            .await?;
        let key = get_chat_admin_cache_key(self.get_id());
        REDIS.sq(|q| q.hdel(&key, user)).await?;
        Ok(())
    }

    async fn refresh_cached_admins(&self) -> Result<HashMap<i64, ChatMember>> {
        if let Err(_) = is_group_or_die(self).await {
            return Ok(HashMap::new());
        }

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
                    q.del(&key);
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
            Err(BotError::speak(lang_fmt!(lang, "cachewait"), self.get_id()))
        }
    }
}

/// Fails with a printable error if the bot doesn't have full admin rights
pub async fn self_admin_or_die(chat: &Chat) -> Result<()> {
    if !is_self_admin(chat).await? {
        let lang = get_chat_lang(chat.get_id()).await?;
        Err(BotError::speak(
            lang_fmt!(lang, "needtobeadmin"),
            chat.get_id(),
        ))
    } else {
        Ok(())
    }
}

fn get_chat_admin_cache_key(chat: i64) -> String {
    format!("ca:{}", chat)
}
