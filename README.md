# Discord Bot ReRusted 🦀

A Discord bot rebuilt in Rust with Poise and a Hugging Face Inference Providers AI backend.

## Current commands

- `/ping` — checks that the bot is alive.
- `/ask <prompt>` — sends a prompt to the configured Hugging Face model and returns the response.
- `/remember <fact>` — stores a user-specific fact in local VxMem storage.

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

Direct messages are always AI-enabled and do not require a mention. An empty `AI_CHANNEL_IDS` disables automatic guild mention replies but does not disable DMs.

Mentions in non-enabled guild channels are ignored and recorded in the bot log with the channel ID, author, and cleaned message text. The bot does not post an error into the non-enabled channel.

Poise's mention-as-prefix behavior is explicitly disabled, so `@ReRusted hello` is handled only by the custom message event handler rather than being parsed as an unknown prefix command.

Because Discord message text is delivered through the **Message Content** gateway intent, enable the Message Content Intent for the bot in the Discord Developer Portal as well as requesting it in the code.

## VxMem

Vaxxer has a local memory store named **VxMem** using the custom `.vxm` format.

By default, for this project we recommend keeping it outside the repository:

```env
VXM_PATH=/home/vexx/.local/share/rerusted/memory.vxm
VXM_HISTORY_LIMIT=50
```

The `.vxm` file stores recent conversation messages separately for each DM/user or guild channel, plus long-term facts. User messages that look like durable personal facts are automatically promoted to long-term memory without making another AI request.

VXM/2 is human-readable. Strings use normal quoted/escaped text so the file can be inspected and edited in a text editor. Existing VXM/1 files are still readable and are converted to VXM/2 the next time VxMem saves.

The file is local-only and is intentionally ignored by Git. It is written through a temporary file and rename so a partial write is less likely to destroy the existing memory file.

VxMem is loaded when the bot starts and saved whenever memory changes.

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
- A Hugging Face token with **Inference Providers** permission

## Local setup

```bash
cp .env.example .env
```

Put your credentials and settings in `.env`:

```env
DISCORD_TOKEN=your_discord_token
HF_TOKEN=your_huggingface_token
HF_MODEL=openai/gpt-oss-120b:fastest
AI_CHANNEL_IDS=123456789012345678,987654321098765432
VXM_PATH=/home/vexx/.local/share/rerusted/memory.vxm
VXM_HISTORY_LIMIT=50
AI_CONTEXT_FILE=context.md
```

Then run:

```bash
cargo run
```

## AI provider

The bot uses Hugging Face's OpenAI-compatible Inference Providers chat-completions endpoint. The model can be changed without recompiling:

```env
HF_MODEL=your-model-id:fastest
```

The application strips leaked safety-classifier metadata such as `User Safety:` / `Safety Categories:` lines instead of posting those internal-looking labels into Discord. When such metadata is returned, Vaxxer replies with `fuck off`.

Never commit `.env`, `memory.vxm`, or API tokens to Git.
