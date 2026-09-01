use std::{env, sync::Arc};

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
}

#[derive(Clone, Debug)]
struct OpenRouterClient {
    http: Client,
    api_key: String,
    model: String,
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

    async fn chat(&self, prompt: &str) -> Result<String, Error> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
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

#[poise::command(slash_command, prefix_command)]
async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("🏓 Pong! ReRusted is alive.").await?;
    Ok(())
}

#[poise::command(slash_command, prefix_command)]
async fn ask(
    ctx: Context<'_>,
    #[description = "What should the bot ask the AI?"] prompt: String,
) -> Result<(), Error> {
    ctx.defer().await?;

    match ctx.data().openrouter.chat(&prompt).await {
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

    let intents = serenity::GatewayIntents::non_privileged();
    let commands = vec![ping(), ask()];

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            on_error: |error| Box::pin(async move {
                error!("Poise command error: {error:?}");
            }),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            let openrouter = Arc::clone(&openrouter);
            Box::pin(async move {
                info!(user = %ready.user.name, "Connected to Discord");
                info!(guilds = ready.guilds.len(), "Bot is serving guilds");

                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                Ok(Data { openrouter })
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
