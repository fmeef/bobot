use crate::tg::command::Context;
use crate::tg::user::Username;
use crate::util::error::BotError;
use crate::util::string::Lang;
use crate::{
    metadata::metadata,
    tg::admin_helpers::*,
    tg::command::{Entities, TextArgs},
    tg::permissions::*,
    util::error::Result,
    util::string::Speak,
};
use botapi::gen_types::{Message, UpdateExt};

use futures::FutureExt;
use humantime::format_duration;
use macros::lang_fmt;
use sea_orm_migration::MigrationTrait;

metadata!("Warns",
    r#"
    Keep your users in line with warnings! Good for pressuring people not to say the word "bro"
    "#,
    { command = "warn", help = "Warns a user"},
    { command = "warns", help = "Get warn count of a user"},
    { command = "clearwarns", help = "Delete all warns for a user"},
    { command = "warntime", help = "Sets time before warns expire. Usage: /warntime 6m for 6 minutes"},
    { command = "warnmode", help = "Set the action when max warns are reached. Can be 'mute', 'ban' or 'shame'"}
);

pub fn get_migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![]
}
pub async fn warn<'a>(
    message: &Message,
    entities: &Entities<'a>,
    args: &TextArgs<'a>,
    lang: Lang,
) -> Result<()> {
    message.group_admin_or_die().await?;

    action_message(message, entities, Some(args), |message, user, args| {
        async move {
            if user.is_admin(message.get_chat_ref()).await? {
                return Err(BotError::speak(
                    &lang_fmt!(lang, "warnadmin"),
                    message.get_chat().get_id(),
                ));
            }

            let reason = args
                .map(|a| {
                    if a.args.len() > 0 {
                        Some(a.text.trim())
                    } else {
                        None
                    }
                })
                .flatten();

            warn_with_action(message, user, reason, None).await?;
            Ok(())
        }
        .boxed()
    })
    .await?;
    Ok(())
}

pub async fn warns<'a>(message: &Message, entities: &Entities<'a>, lang: Lang) -> Result<()> {
    is_group_or_die(&message.get_chat()).await?;
    self_admin_or_die(&message.get_chat()).await?;

    action_message(message, entities, None, |message, user, _| {
        async move {
            let warns = get_warns(message, user).await?;
            let list = warns
                .into_iter()
                .map(|w| {
                    format!(
                        "Reason: {}",
                        w.reason.unwrap_or_else(|| lang_fmt!(lang, "noreason"))
                    )
                })
                .collect::<Vec<String>>()
                .join("\n");
            message
                .reply(lang_fmt!(lang, "warns", user.name_humanreadable(), list))
                .await?;
            Ok(())
        }
        .boxed()
    })
    .await?;
    Ok(())
}

pub async fn clear<'a>(message: &Message, entities: &Entities<'a>) -> Result<()> {
    is_group_or_die(&message.get_chat()).await?;
    self_admin_or_die(&message.get_chat()).await?;
    message
        .get_from()
        .admin_or_die(message.get_chat_ref())
        .await?;
    action_message(message, entities, None, |message, user, _| {
        async move {
            clear_warns(message.get_chat_ref(), user).await?;

            let name = user
                .get_username()
                .unwrap_or_else(|| std::borrow::Cow::Owned(user.get_id().to_string()));
            message
                .reply(format!("Cleared warns for user {}", name))
                .await?;
            Ok(())
        }
        .boxed()
    })
    .await?;

    Ok(())
}

async fn set_time<'a>(message: &Message, args: &TextArgs<'a>) -> Result<()> {
    message.group_admin_or_die().await?;
    if let Some(time) = parse_duration(&Some(args.as_slice()), message.get_chat().get_id())? {
        set_warn_time(message.get_chat_ref(), time.num_seconds()).await?;
        let time = format_duration(time.to_std()?);
        message.reply(format!("Set warn time to {}", time)).await?;
    } else {
        message.reply("Specify a time").await?;
    }
    Ok(())
}

async fn cmd_warn_mode<'a>(message: &Message, args: &TextArgs<'a>) -> Result<()> {
    message.group_admin_or_die().await?;
    set_warn_mode(message.get_chat_ref(), args.text).await?;
    message
        .reply(format!("Set warn mode {}", args.text))
        .await?;
    Ok(())
}

async fn handle_command<'a>(ctx: &Context<'a>) -> Result<()> {
    if let Some((cmd, entities, args, message, lang)) = ctx.cmd() {
        match cmd {
            "warn" => warn(message, &entities, args, lang.clone()).await,
            "warns" => warns(message, &entities, lang.clone()).await,
            "clearwarns" => clear(message, &entities).await,
            "warntime" => set_time(message, args).await,
            "warnmode" => cmd_warn_mode(message, args).await,
            _ => Ok(()),
        }?;
    }
    Ok(())
}

pub async fn handle_update<'a>(_: &UpdateExt, cmd: &Option<Context<'a>>) -> Result<()> {
    if let Some(cmd) = cmd {
        handle_command(cmd).await?;
    }
    Ok(())
}