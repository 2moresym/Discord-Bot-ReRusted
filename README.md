# Discord Bot ReRusted 🦀

A Discord bot rebuilt in Rust with **Poise** and an **OpenRouter-powered AI backend**.

## Current commands

- `/ping` — checks that the bot is alive.
- `/ask <prompt>` — sends a prompt to the configured OpenRouter model and returns the response.

Commands are slash-only. Mentions are handled separately and are never treated as prefix commands.

## AI mention replies

ReRusted can reply automatically when it is mentioned in selected Discord channels.

Configure channel IDs in `.env`:

```env
AI_CHANNEL_IDS=1363801874465161316,123456789012345678
```

Channel IDs are globally unique, so the list can contain channels from multiple Discord servers.

The bot only automatically answers when all of these are true:

1. The message is in one of the configured channel IDs.
2. The message mentions ReRusted.
3. The message was not sent by another bot.

Direct messages are ignored. An empty `AI_CHANNEL_IDS` disables automatic mention replies.

Mentions in non-enabled channels are ignored and recorded in the bot log with the channel ID, author, and cleaned message text. The bot does not post an error into the non-enabled channel.

Poise's mention-as-prefix behavior is explicitly disabled, so `@ReRusted hello` is handled only by the custom message event handler rather than being parsed as an unknown prefix command.

Because Discord message text is delivered through the **Message Content** gateway intent, enable the Message Content Intent for the bot in the Discord Developer Portal as well as requesting it in the code.

## Vaxxer context

Vaxxer's personality and long-form instructions live in [`context.md`](context.md), separate from `.env`.

By default the bot loads `context.md` at startup. You can point it at another file with:

```env
AI_CONTEXT_FILE=context.md
```

The context file is read once when the bot starts, so restart the bot after editing it.

## Requirements

- Rust stable
- A Discord bot token
- An OpenRouter API key

## Local setup

```bash
cp .env.example .env
```

Put your credentials and settings in `.env`:

```env
DISCORD_TOKEN=your_discord_token
OPENROUTER_API_KEY=your_openrouter_key
OPENROUTER_MODEL=openrouter/free
AI_CHANNEL_IDS=123456789012345678,987654321098765432
AI_CONTEXT_FILE=context.md
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

The application also strips the provider/model's stray `User safety=safe` line if it appears in a response, so internal metadata does not get posted into Discord.

Never commit `.env` or API tokens to Git.
