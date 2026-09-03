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

Vaxxer has a local memory system named **VxMem** using the custom `.vxm` format.

Recommended local storage for this machine:

```env
VXM_PATH=/home/vexx/.local/share/rerusted/memory.vxm
VXM_HISTORY_LIMIT=50
VXM_MAX_MESSAGES=10000
```

`VXM_HISTORY_LIMIT` controls how many newest messages from the active conversation are always included. `VXM_MAX_MESSAGES` controls how many messages are retained on disk per conversation scope. Older retained messages are not automatically sent to the AI; VxMem searches them and selects relevant matches for the current prompt.

VxMem maintains three useful layers:

1. **Recent context** — the newest messages from the current DM or guild channel.
2. **Relevant history** — older messages from the same conversation ranked using token overlap and recency.
3. **Long-term memory** — durable user facts with importance scores and duplicate detection.

Personal facts can be promoted automatically from normal user messages without spending another AI request. `/remember <fact>` is still available for facts that should be stored explicitly.

The current VxMem format is **VXM/3** and is human-readable. Strings use normal quoted/escaped text, timestamps and importance are visible, and the file can be inspected in a text editor. Existing VXM/1 and VXM/2 files are still readable and are rewritten as VXM/3 on the next save.

VxMem creates missing parent directories, saves through a temporary file and rename, and keeps the memory store outside Git. The `.vxm` file is intentionally ignored by the repository.

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
VXM_MAX_MESSAGES=10000
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
