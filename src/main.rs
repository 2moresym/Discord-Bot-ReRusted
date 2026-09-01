use std::{env, sync::Arc};

use dotenvy::dotenv;
use poise::serenity_prelude as serenity;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Clone)]
struct Data {
    groq: Arc<GroqClient>,
}

#[derive(Clone)]
struct GroqClient {
    http: Client,
    api_key: String,
    model: String,
}

impl GroqClient {
    fn from_env() -> Result<Self, Error> {
        let api_key = env::var("GROQ_API_KEY")
            .map_err(|_| "GROQ_API_KEY is missing from the environment")?;
        let model = env::var("GROQ_MODEL").unwrap_or_else(|_| "openai/gpt-oss-20b".to_owned());

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
            .post("https://api.groq.com/openai/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(format!("Groq API returned {status}: {body}").into());
        }

        let parsed: ChatResponse = serde_json::from_str(&body)?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| "Groq returned no choices".into())
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
    #[description = "What should the bot ask Groq?"] prompt: String,
) -> Result<(), Error> {
    ctx.defer().await?;

    match ctx.data().groq.chat(&prompt).await {
        Ok(answer) => {
            // Discord messages have a 2000-character limit.
            let answer = truncate_for_discord(&answer);
            ctx.say(answer).await?;
        }
        Err(err) => {
            error!(error = %err, "Groq request failed");
            ctx.say("❌ Groq failed to answer. Check the bot logs for details.")
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
    let groq = Arc::new(GroqClient::from_env()?);

    let intents = serenity::GatewayIntents::non_privileged();
    let commands = vec![ping(), ask()];

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            on_error: |error| Box::pin(async move {
                error!(error = ?error, "Poise command error");
            }),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            let groq = Arc::clone(&groq);
            Box::pin(async move {
                info!(user = %ready.user.name, "Connected to Discord");
                info!(guilds = ready.guilds.len(), "Bot is serving guilds");

                // Register slash commands globally. Discord may take up to an hour
                // to propagate global command changes.
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;

                Ok(Data { groq })
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
