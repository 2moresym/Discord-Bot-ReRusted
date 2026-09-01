# Discord Bot ReRusted 🦀

A Discord bot rebuilt in Rust with **Poise** and an **OpenRouter-powered AI backend**.

## Current commands

- `/ping` — checks that the bot is alive.
- `/ask <prompt>` — sends a prompt to the configured OpenRouter model and returns the response.

## AI mention replies

ReRusted can also reply automatically when it is mentioned in selected Discord channels.

Configure channel names in `.env`:

```env
AI_CHANNEL_NAMES=ai-chat,bot-chat
```

Channel names are matched case-insensitively. The bot only replies when all of these are true:

1. The message is in a configured channel name.
2. The message mentions ReRusted.
3. The message was not sent by another bot.

Direct messages are ignored. An empty `AI_CHANNEL_NAMES` disables automatic mention replies.

Because Discord message text is delivered through the **Message Content** gateway intent, enable the Message Content Intent for the bot in the Discord Developer Portal as well as requesting it in the code.

## Requirements

- Rust stable
- A Discord bot token
- An OpenRouter API key

Poise 0.6 supports event handlers for non-command Discord events, which ReRusted uses for mention-based replies. citeturn199351search1

## Local setup

```bash
cp .env.example .env
```

Put your credentials in `.env`:

```env
DISCORD_TOKEN=your_discord_token
OPENROUTER_API_KEY=your_openrouter_key
OPENROUTER_MODEL=openrouter/free
AI_CHANNEL_NAMES=ai-chat
```

`HF_TOKEN` is included as an optional placeholder for a future fallback provider; it is not used by the current bot core.

Then run:

```bash
cargo run
```

## AI provider

The bot uses OpenRouter's OpenAI-compatible chat-completions endpoint. The model can be changed without recompiling:

```env
OPENROUTER_MODEL=your-model-id
```

Never commit `.env` or API tokens to Git.
