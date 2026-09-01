# Discord Bot ReRusted 🦀

A Discord bot rebuilt in Rust with **Poise** and a **Groq-powered AI backend**.

## Current commands

- `/ping` — checks that the bot is alive.
- `/ask <prompt>` — sends a prompt to Groq and returns the model response.

## Requirements

- Rust stable
- A Discord bot token
- A Groq API key

Poise 0.6 provides slash-command support through the `#[poise::command]` macro, and this project uses Groq's OpenAI-compatible chat-completions endpoint. citeturn187917search0turn556785search0

## Local setup

```bash
cp .env.example .env
```

Put your credentials in `.env`:

```env
DISCORD_TOKEN=your_discord_token
GROQ_API_KEY=your_groq_key
GROQ_MODEL=openai/gpt-oss-20b
```

`HF_TOKEN` is included as an optional placeholder for a future fallback provider; it is not used by the current bot core.

Then run:

```bash
cargo run
```

## AI provider

Groq currently exposes an OpenAI-compatible API at `https://api.groq.com/openai/v1`. The default model here is `openai/gpt-oss-20b`, which Groq currently lists as a production model. citeturn556785search0turn556785search1

The model can be changed without recompiling:

```env
GROQ_MODEL=your-model-id
```

Never commit `.env` or API tokens to Git.
