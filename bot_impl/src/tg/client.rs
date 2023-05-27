//! Telegram client wrapper with webhook support. Handles incomming updates from telegram
//! and forwards them to modules. This type indexes module metadata and autogenerates a help
//! command handler as well. Due to rust async limitations with the borrow checker this type
//! is most useful from a static context only

use std::collections::HashMap;

use botapi::{
    bot::{ApiError, Bot},
    ext::{BotUrl, LongPoller, Webhook},
    gen_types::{
        CallbackQuery, InlineKeyboardButton, InlineKeyboardButtonBuilder, Message, UpdateExt,
    },
};
use dashmap::DashMap;
use macros::{lang_fmt, message_fmt};

use super::{
    admin_helpers::{handle_pending_action, is_dm},
    button::{get_url, InlineKeyboardBuilder},
    dialog::{Conversation, ConversationState},
    permissions::*,
    user::RecordUser,
};
use crate::{
    metadata::Metadata,
    modules,
    statics::ME,
    util::{
        callback::{MultiCallback, MultiCb, SingleCallback, SingleCb},
        error::BotError,
        string::should_ignore_chat,
    },
};
use crate::{
    statics::{CONFIG, TG},
    util::error::Result,
    util::string::get_chat_lang,
};
use futures::{Future, StreamExt};
use std::sync::Arc;

static INVALID: &str = "invalid";

pub struct MetadataCollection {
    pub helps: HashMap<String, String>,
    pub modules: HashMap<String, Metadata>,
}

impl MetadataCollection {
    fn get_module_text(&self, module: &str) -> String {
        self.modules
            .get(module)
            .map(|v| {
                let helps = v
                    .commands
                    .iter()
                    .map(|(c, h)| format!("/{}: {}", c, h))
                    .collect::<Vec<String>>()
                    .join("\n");
                format!("[*{}]:\n{}\n\nCommands:\n{}", v.name, v.description, helps)
            })
            .unwrap_or_else(|| INVALID.to_owned())
    }

    pub async fn get_conversation(&self, message: &Message) -> Result<Conversation> {
        let me = ME.get().unwrap();

        let lang = get_chat_lang(message.get_chat().get_id()).await?;
        let mut state = ConversationState::new_prefix(
            "help".to_owned(),
            lang_fmt!(lang, "welcome", me.get_first_name()),
            message.get_chat().get_id(),
            message.get_from().map(|u| u.get_id()).ok_or_else(|| {
                BotError::speak("User does not exist", message.get_chat().get_id())
            })?,
            "button",
        )?;

        let start = state.get_start()?.state_id;
        self.modules.iter().for_each(|(_, n)| {
            let s = state.add_state(self.get_module_text(&n.name));
            state.add_transition(start, s, n.name.clone());
            state.add_transition(s, start, "Back");
        });

        let conversation = state.build();
        conversation.write_self().await?;
        Ok(conversation)
    }
}

pub struct TgClient {
    pub client: Bot,
    pub modules: Arc<MetadataCollection>,
    pub button_events: Arc<DashMap<String, SingleCb<CallbackQuery, Result<()>>>>,
    pub button_repeat: Arc<DashMap<String, MultiCb<CallbackQuery, Result<bool>>>>,
}

pub async fn show_help<'a>(message: &Message, helps: Arc<MetadataCollection>) -> Result<bool> {
    if !should_ignore_chat(message.get_chat().get_id()).await? {
        let lang = get_chat_lang(message.get_chat().get_id()).await?;
        if is_dm(message.get_chat_ref()) {
            let me = ME.get().unwrap();
            TG.client()
                .build_send_message(
                    message.get_chat().get_id(),
                    &lang_fmt!(lang, "welcome", me.get_first_name()),
                )
                .reply_markup(&botapi::gen_types::EReplyMarkup::InlineKeyboardMarkup(
                    helps
                        .get_conversation(&message)
                        .await?
                        .get_current_markup(3)
                        .await?,
                ))
                .reply_to_message_id(message.get_message_id())
                .build()
                .await?;
        } else {
            let url = get_url("help")?;
            let mut button = InlineKeyboardBuilder::default();

            button.button(
                InlineKeyboardButtonBuilder::new("Inbix!".to_owned())
                    .set_url(url)
                    .build(),
            );
            message_fmt!(lang, message.get_chat().get_id(), "dmhelp")
                .reply_markup(&botapi::gen_types::EReplyMarkup::InlineKeyboardMarkup(
                    button.build(),
                ))
                .build()
                .await?;
        }
    }

    Ok(true)
}

impl TgClient {
    /// Register a button callback to be called when the corresponding callback button sends an update
    /// This callback will only fire once and be removed afterwards
    pub fn register_button<F, Fut>(&self, button: &InlineKeyboardButton, func: F)
    where
        F: FnOnce(CallbackQuery) -> Fut + Sync + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        if let Some(data) = button.get_callback_data() {
            log::info!("registering button callback with data {}", data);
            self.button_events
                .insert(data.into_owned(), SingleCb::new(func));
        }
    }

    /// Register a button callback to be called when the corresponding callback button sends an update
    /// This callback will be called any number of times until the callback returns false
    pub fn register_button_multi<F, Fut>(&self, button: &InlineKeyboardButton, func: F)
    where
        F: Fn(CallbackQuery) -> Fut + Sync + Send + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        if let Some(data) = button.get_callback_data() {
            log::info!("registering button callback with data {}", data);
            self.button_repeat
                .insert(data.into_owned(), MultiCb::new(func));
        }
    }

    /// Creates a new client from a bot api token
    pub fn connect<T>(token: T) -> Self
    where
        T: Into<String>,
    {
        let metadata = modules::get_metadata();
        let metadata = MetadataCollection {
            helps: metadata
                .iter()
                .flat_map(|v| v.commands.iter())
                .map(|(c, h)| (c.to_owned(), h.to_owned()))
                .collect(),
            modules: metadata.into_iter().map(|v| (v.name.clone(), v)).collect(),
        };
        Self {
            client: Bot::new(token).unwrap(),
            modules: Arc::new(metadata),
            button_events: Arc::new(DashMap::new()),
            button_repeat: Arc::new(DashMap::new()),
        }
    }

    /// Processes a single update from telegram
    async fn handle_update(&self, update: std::result::Result<UpdateExt, ApiError>) {
        let modules = Arc::clone(&self.modules);
        let callbacks = Arc::clone(&self.button_events);
        let repeats = Arc::clone(&self.button_repeat);
        tokio::spawn(async move {
            match update {
                Ok(UpdateExt::CallbackQuery(callbackquery)) => {
                    if let Some(data) = callbackquery.get_data() {
                        let data: String = data.into_owned();
                        if let Some(cb) = callbacks.remove(&data) {
                            if let Err(err) = cb.1.cb(callbackquery.clone()).await {
                                log::error!("button handler err {}", err);
                                err.record_stats();
                            }
                        }

                        let remove = if let Some(cb) = repeats.get(&data) {
                            match cb.cb(callbackquery).await {
                                Err(err) => {
                                    log::error!("failed multi handler {}", err);
                                    err.record_stats();
                                    true
                                }
                                Ok(v) => {
                                    if v {
                                        log::info!("removing multi callback");
                                    }
                                    v
                                }
                            }
                        } else {
                            false
                        };

                        if remove {
                            repeats.remove(&data);
                        }
                    }
                }
                Ok(update) => {
                    if let Err(err) = update_self_admin(&update).await {
                        log::error!("failed to update admin change: {}", err);
                        err.record_stats();
                    }
                    if let Err(err) = handle_pending_action(&update).await {
                        log::error!("failed to handle pending action: {}", err);
                        err.record_stats();
                    }
                    if let Err(err) = update.record_user().await {
                        log::error!("failed to record_user: {}", err);
                        err.record_stats();
                    }

                    if let Err(err) = crate::modules::process_updates(update, modules).await {
                        log::error!("process updates error: {}", err);
                        err.record_stats()
                    }
                }
                Err(err) => {
                    log::error!("failed to process update: {}", err);
                }
            }
        });
    }

    /// Handles updates from telegram forever either using webhooks or long polling
    /// depending on toml config
    pub async fn run(&self) -> Result<()> {
        log::info!("run");
        let updates = Some(
            vec![
                "update_id",
                "message",
                "edited_message",
                "channel_post",
                "edited_channel_post",
                "inline_query",
                "chosen_inline_result",
                "callback_query",
                "shipping_query",
                "pre_checkout_query",
                "poll",
                "poll_answer",
                "my_chat_member",
                "chat_member",
                "chat_join_request",
            ]
            .into_iter()
            .map(|v| v.to_owned())
            .collect(),
        );
        match CONFIG.webhook.enable_webhook {
            false => {
                self.client
                    .build_delete_webhook()
                    .drop_pending_updates(true) // TODO: change this
                    .build()
                    .await?;
                LongPoller::new(&self.client, updates)
                    .get_updates()
                    .await
                    .for_each_concurrent(
                        None,
                        |update| async move { self.handle_update(update).await },
                    )
                    .await
            }
            true => {
                Webhook::new(
                    &self.client,
                    BotUrl::Host(CONFIG.webhook.webhook_url.to_owned()),
                    false,
                    CONFIG.webhook.listen.to_owned(),
                    updates,
                )
                .get_updates()
                .await?
                .for_each_concurrent(
                    None,
                    |update| async move { self.handle_update(update).await },
                )
                .await
            }
        }
        Ok(())
    }

    pub fn client<'a>(&'a self) -> &'a Bot {
        &self.client
    }
}

impl Clone for TgClient {
    fn clone(&self) -> Self {
        TgClient {
            client: self.client.clone(),
            modules: Arc::clone(&self.modules),
            button_events: Arc::clone(&self.button_events),
            button_repeat: Arc::clone(&self.button_repeat),
        }
    }
}
