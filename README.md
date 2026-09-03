# Discord Bot ReRusted 🦀

A Discord bot rebuilt in Rust with **Poise** and a **Cerebras Inference** AI backend.

## Current commands

- `/ping` — checks that the bot is alive.
- `/ask <prompt>` — sends a prompt to the configured Cerebras model and returns the response.
- `/remember <fact>` — stores a user-specific fact in local VxMem storage.

Commands are slash-only. Mentions are handled separately and are never treated as prefix commands.

## AI mention replies

ReRusted can reply automatically when it is mentioned in selected Discord channels.

Configure channel IDs in `.env`:

```env
AI_CHANNEL_IDS=1363801874465161316,123456789012345678
```

The list can contain channels from multiple Discord servers.

Guild AI replies require all of these:

1. The message is in one of the configured channel IDs.
2. The message mentions ReRusted.
3. The message was not sent by another bot.

Direct messages are always AI-enabled and do not require a mention. DM replies are sent normally without mentioning the user.

Mentions in non-enabled guild channels are ignored and logged with the channel ID, author, and cleaned message text. Nothing is posted back into that channel.

Poise's mention-as-prefix behavior is explicitly disabled, so `@ReRusted hello` is handled only by the custom message event handler.

Because Discord message text is delivered through the **Message Content** gateway intent, enable the Message Content Intent for the bot in the Discord Developer Portal as well as requesting it in code.

## VxMem

Vaxxer has a local memory system named **VxMem** using the custom `.vxm` format.

Recommended storage for this machine:

```env
VXM_PATH=/home/vexx/.local/share/rerusted/memory.vxm
VXM_HISTORY_LIMIT=50
VXM_MAX_MESSAGES=10000
```

`VXM_HISTORY_LIMIT` controls how many newest messages from the active conversation are always included. `VXM_MAX_MESSAGES` controls how many messages are retained on disk per conversation scope. Older retained messages are searched and ranked instead of being dumped into every prompt.

VxMem has four layers:

1. **Recent context** — newest messages from the current DM or guild channel.
2. **Relevant history** — older messages selected using token overlap, phrase matching, and recency.
3. **Long-term memory** — durable user facts with importance, confidence, access counts, categories, and tags.
4. **Reinforcement** — memories that are repeatedly retrieved gain usage metadata, helping frequently useful facts stay relevant.

Personal facts can be promoted automatically from normal user messages without another AI request. `/remember <fact>` remains available for explicit memories.

The current format is **VXM/4** and is human-readable. Existing VXM/1, VXM/2, and VXM/3 files remain readable and are rewritten in VXM/4 on the next save.

The store creates missing parent directories, writes through a temporary file and rename, and stays outside Git. The actual `.vxm` memory file is intentionally ignored by the repository.

## Vaxxer context

Vaxxer's personality and long-form instructions live in [`context.md`](context.md), separate from `.env`.

The context file is read at startup, so restart the bot after editing it.

## Requirements

- Rust stable
- A Discord bot token
- A Cerebras API key

## Local setup

```bash
cp .env.example .env
```

Put your credentials and settings in `.env`:

```env
DISCORD_TOKEN=your_discord_token
CEREBRAS_API_KEY=your_cerebras_api_key
CEREBRAS_MODEL=gpt-oss-120b
CEREBRAS_REASONING_EFFORT=medium
CEREBRAS_MAX_COMPLETION_TOKENS=512
AI_CHANNEL_IDS=123456789012345678,987654321098765432
VXM_PATH=/home/vexx/.local/share/rerusted/memory.vxm
VXM_HISTORY_LIMIT=50
VXM_MAX_MESSAGES=10000
AI_CONTEXT_FILE=context.md
```

Then run:

```bash
cargo run
```

## AI provider

The bot uses Cerebras' OpenAI-compatible chat-completions API at `https://api.cerebras.ai/v1/chat/completions`. The default model is `gpt-oss-120b`. Cerebras documents `gpt-oss-120b` reasoning controls for `low`, `medium`, and `high`; the bot exposes that setting through `CEREBRAS_REASONING_EFFORT`. Completion length is bounded through `CEREBRAS_MAX_COMPLETION_TOKENS`.

## Safety metadata handling

The application strips leaked safety-classifier metadata such as `User Safety:` / `Safety Categories:` lines instead of posting those labels into Discord. When such metadata is returned, Vaxxer replies with `fuck off`.

Never commit `.env`, `.vxm`, memory data, or API tokens to Git.
