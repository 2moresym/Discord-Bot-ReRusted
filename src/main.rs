use std::{
    collections::HashSet,
    env,
    fmt,
    fs,
    sync::Arc,
    time::Duration,
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
    huggingface: Arc<HuggingFaceClient>,
    ai_channel_ids: HashSet<serenity::ChannelId>,
    system_prompt: Arc<String>,
}

#[derive(Clone)]
struct HuggingFaceClient {
    http: Client,
    api_key: String,
    model: String,
}

impl fmt::Debug for HuggingFaceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HuggingFaceClient")
            .field("http", &self.http)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .finish()
    }
}

impl HuggingFaceClient {
    fn from_env() -> Result<Self, Error> {
        let api_key = env::var("HF_TOKEN")
            .map_err(|_| "HF_TOKEN is missing from the environment")?;
        let model = env::var("HF_MODEL")
            .unwrap_or_else(|_| "openai/gpt-oss-120b:fastest".to_owned());

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
            .post("https://router.huggingface.co/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .header("X-Title", "Discord Bot ReRusted")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(format!("Hugging Face API returned {status}: {body}").into());
        }

        let parsed: ChatResponse = serde_json::from_str(&body)?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| sanitize_ai_response(&choice.message.content))
            .ok_or_else(|| "Hugging Face returned no choices".into())
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

fn load_ai_channel_ids() -> Result<HashSet<serenity::ChannelId>, Error> {
    let configured = env::var("AI_CHANNEL_IDS").unwrap_or_default();
    let mut channels = HashSet::new();

    for raw_id in configured.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        let id = raw_id
            .parse::<u64>()
            .map_err(|_| format!("Invalid channel ID in AI_CHANNEL_IDS: {raw_id}"))?;
        channels.insert(serenity::ChannelId::new(id));
    }

    Ok(channels)
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

fn sanitize_ai_response(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();

    let has_safety_metadata = lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("user safety:")
            || lower.starts_with("user safety=")
            || lower.starts_with("safety categories:")
            || lower.starts_with("safety categories=")
            || lower.starts_with("response safety:")
            || lower.starts_with("response safety=")
    });

    if has_safety_metadata {
        return "fuck off".to_owned();
    }

    let cleaned = lines.join("\n");
    if cleaned.is_empty() {
        "...the AI returned an empty response. What the fuck.".to_owned()
    } else {
        cleaned
    }
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

    match ctx.data().huggingface.chat(&ctx.data().system_prompt, &prompt).await {
        Ok(answer) => {
            let answer = truncate_for_discord(&answer);
            ctx.say(answer).await?;
        }
        Err(err) => {
            error!(error = %err, "Hugging Face request failed");
            ctx.say("the ai provider fucked up. check the logs.").await?;
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
    let huggingface = Arc::new(HuggingFaceClient::from_env()?);
    let ai_channel_ids = load_ai_channel_ids()?;
    let system_prompt = Arc::new(load_system_prompt()?);

    if ai_channel_ids.is_empty() {
        info!("AI mention replies are disabled because AI_CHANNEL_IDS is empty");
    } else {
        info!(channels = ?ai_channel_ids, "AI mention replies enabled for channel IDs");
    }

    let intents = serenity::GatewayIntents::non_privileged()
        | serenity::GatewayIntents::MESSAGE_CONTENT;
    let commands = vec![ping(), ask()];

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            prefix_options: poise::PrefixFrameworkOptions {
                mention_as_prefix: false,
                ..Default::default()
            },
            on_error: |error| Box::pin(async move {
                error!("Poise command error: {error}");
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

                    if !data.ai_channel_ids.contains(&new_message.channel_id) {
                        let clean_message = strip_bot_mention(&new_message.content, bot_id);
                        info!(
                            channel_id = %new_message.channel_id.get(),
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

                    let typing = new_message.channel_id.start_typing(&_ctx.http);
                    let response = tokio::time::timeout(
                        Duration::from_secs(45),
                        data.huggingface.chat(&data.system_prompt, &prompt),
                    )
                    .await;
                    typing.stop();

                    match response {
                        Ok(Ok(answer)) => {
                            let answer = truncate_for_discord(&answer);
                            new_message.reply_mention(_ctx, answer).await?;
                        }
                        Ok(Err(err)) => {
                            error!(error = %err, "Hugging Face mention reply failed");
                            new_message
                                .reply_mention(
                                    _ctx,
                                    "the ai provider fucked up. check the logs.",
                                )
                                .await?;
                        }
                        Err(_) => {
                            error!("Hugging Face mention reply timed out after 45 seconds");
                            new_message
                                .reply_mention(
                                    _ctx,
                                    "the ai took too long to answer. what the fuck happened?",
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
            let huggingface = Arc::clone(&huggingface);
            let ai_channel_ids = ai_channel_ids.clone();
            let system_prompt = Arc::clone(&system_prompt);
            Box::pin(async move {
                info!(user = %ready.user.name, "Connected to Discord");
                info!(guilds = ready.guilds.len(), "Bot is serving guilds");

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                Ok(Data {
                    huggingface,
                    ai_channel_ids,
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
