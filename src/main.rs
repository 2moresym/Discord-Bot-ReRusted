use std::{
    collections::HashSet,
    env,
    fmt,
    fs,
    sync::Arc,
};

use dotenvy::dotenv;
use poise::serenity_prelude as serenity;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Clone, Debug)]
struct Data {
    openrouter: Arc<OpenRouterClient>,
    ai_channel_names: HashSet<String>,
    system_prompt: Arc<String>,
}

#[derive(Clone)]
struct OpenRouterClient {
    http: Client,
    api_key: String,
    model: String,
}

impl fmt::Debug for OpenRouterClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenRouterClient")
            .field("http", &self.http)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .finish()
    }
}

impl OpenRouterClient {
    fn from_env() -> Result<Self, Error> {
        let api_key = env::var("OPENROUTER_API_KEY")
            .map_err(|_| "OPENROUTER_API_KEY is missing from the environment")?;
        let model = env::var("OPENROUTER_MODEL")
            .unwrap_or_else(|_| "openrouter/free".to_owned());

        Ok(Self {
            http: Client::new(),
            api_key,
            model,
        })
    }

    async fn chat(&self, system_prompt: &str, prompt: &str) -> Result<String, Error> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_owned(),
                    content: system_prompt.to_owned(),
                },
                ChatMessage {
                    role: "user".to_owned(),
                    content: prompt.to_owned(),
                },
            ],
        };

        let response = self
            .http
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", "https://github.com/2moresym/Discord-Bot-ReRusted")
            .header("X-Title", "Discord Bot ReRusted")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(format!("OpenRouter API returned {status}: {body}").into());
        }

        let parsed: ChatResponse = serde_json::from_str(&body)?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| "OpenRouter returned no choices".into())
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

fn load_ai_channel_names() -> HashSet<String> {
    env::var("AI_CHANNEL_NAMES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn load_system_prompt() -> Result<String, Error> {
    let path = env::var("AI_CONTEXT_FILE").unwrap_or_else(|_| "context.md".to_owned());
    let prompt = fs::read_to_string(&path)
        .map_err(|err| format!("Failed to read AI context file '{path}': {err}"))?;

    if prompt.trim().is_empty() {
        return Err(format!("AI context file '{path}' is empty").into());
    }

    Ok(prompt)
}

fn strip_bot_mention(content: &str, bot_id: serenity::UserId) -> String {
    let normal = format!("<@{}>", bot_id.get());
    let nickname = format!("<@!{}>", bot_id.get());

    content
        .replace(&normal, "")
        .replace(&nickname, "")
        .trim()
        .to_owned()
}

#[poise::command(slash_command)]
async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("🏓 Pong! ReRusted is alive.").await?;
    Ok(())
}

#[poise::command(slash_command)]
async fn ask(
    ctx: Context<'_>,
    #[description = "What should the bot ask the AI?"] prompt: String,
) -> Result<(), Error> {
    ctx.defer().await?;

    match ctx.data().openrouter.chat(&ctx.data().system_prompt, &prompt).await {
        Ok(answer) => {
            let answer = truncate_for_discord(&answer);
            ctx.say(answer).await?;
        }
        Err(err) => {
            error!(error = %err, "OpenRouter request failed");
            ctx.say("❌ The AI provider failed to answer. Check the bot logs for details.")
                .await?;
        }
    }

    Ok(())
}

fn truncate_for_discord(text: &str) -> String {
    const LIMIT: usize = 2000;

    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }

    let truncated: String = text.chars().take(LIMIT - 3).collect();
    format!("{truncated}...")
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            env::var("RUST_LOG").unwrap_or_else(|_| "discord_bot_rerusted=info".to_owned()),
        )
        .init();

    let token = env::var("DISCORD_TOKEN")
        .map_err(|_| "DISCORD_TOKEN is missing from the environment")?;
    let openrouter = Arc::new(OpenRouterClient::from_env()?);
    let ai_channel_names = load_ai_channel_names();
    let system_prompt = Arc::new(load_system_prompt()?);

    if ai_channel_names.is_empty() {
        info!("AI mention replies are disabled because AI_CHANNEL_NAMES is empty");
    } else {
        info!(channels = ?ai_channel_names, "AI mention replies enabled for channel names");
    }

    let intents = serenity::GatewayIntents::non_privileged()
        | serenity::GatewayIntents::MESSAGE_CONTENT;
    let commands = vec![ping(), ask()];

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            on_error: |error| Box::pin(async move {
                error!("Poise command error: {error:?}");
            }),
            event_handler: |_ctx, event, _framework, data| {
                Box::pin(async move {
                    let serenity::FullEvent::Message { new_message } = event else {
                        return Ok(());
                    };

                    if new_message.author.bot {
                        return Ok(());
                    }

                    let bot_id = _ctx.cache.current_user().id;
                    let mentioned = new_message.mentions.iter().any(|user| user.id == bot_id);

                    if !mentioned {
                        return Ok(());
                    }

                    let Some(guild_id) = new_message.guild_id else {
                        info!(
                            user = %new_message.author.name,
                            "Ignoring AI mention from a DM"
                        );
                        return Ok(());
                    };

                    let channel_name = {
                        let Some(guild) = _ctx.cache.guild(guild_id) else {
                            return Ok(());
                        };

                        let Some(channel) = guild.channels.get(&new_message.channel_id) else {
                            return Ok(());
                        };

                        channel.name.clone()
                    };

                    if !data
                        .ai_channel_names
                        .contains(&channel_name.to_ascii_lowercase())
                    {
                        let clean_message = strip_bot_mention(&new_message.content, bot_id);
                        info!(
                            channel = %channel_name,
                            user = %new_message.author.name,
                            message = %clean_message,
                            "AI mention ignored in non-enabled channel"
                        );
                        return Ok(());
                    }

                    let prompt = strip_bot_mention(&new_message.content, bot_id);
                    let prompt = if prompt.is_empty() {
                        "You were mentioned. Say hello and ask what I can help with.".to_owned()
                    } else {
                        prompt
                    };

                    match data.openrouter.chat(&data.system_prompt, &prompt).await {
                        Ok(answer) => {
                            let answer = truncate_for_discord(&answer);
                            new_message.channel_id.say(_ctx, answer).await?;
                        }
                        Err(err) => {
                            error!(error = %err, "OpenRouter mention reply failed");
                            new_message
                                .channel_id
                                .say(
                                    _ctx,
                                    "❌ The AI provider failed to answer. Check the bot logs for details.",
                                )
                                .await?;
                        }
                    }

                    Ok(())
                })
            },
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            let openrouter = Arc::clone(&openrouter);
            let ai_channel_names = ai_channel_names.clone();
            let system_prompt = Arc::clone(&system_prompt);
            Box::pin(async move {
                info!(user = %ready.user.name, "Connected to Discord");
                info!(guilds = ready.guilds.len(), "Bot is serving guilds");

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                Ok(Data {
                    openrouter,
                    ai_channel_names,
                    system_prompt,
                })
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await?;

    info!("Starting Discord Bot ReRusted");
    client.start().await?;

    Ok(())
}
