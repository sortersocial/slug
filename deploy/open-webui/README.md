# Open WebUI on Fly.io (minimal, OpenRouter)

You do **not** need to host your own model. [OpenRouter](https://openrouter.ai/) runs the LLMs; this Fly app only runs the Open WebUI shell.

### Disk vs RAM

The container image still includes ML libraries on **disk** (upstream wheels). These settings target **runtime memory**: chat and API proxy use remote inference, and `RAG_EMBEDDING_ENGINE=openai` plus `AUDIO_STT_ENGINE=openai` tell Open WebUI to call your configured OpenAI-compatible API for embeddings and speech-to-text instead of loading local SentenceTransformers / Whisper models into RAM (see Open WebUI [performance](https://docs.openwebui.com/troubleshooting/performance/) notes on embedding offload).

You may still see higher RSS if you use features that pull in other local code paths. For a fresh volume, the `[env]` values apply on first boot; if you already ran Open WebUI, stored admin settings can override some env vars ([PersistentConfig](https://docs.openwebui.com/reference/env-configuration/) — adjust in Admin or reset the volume for a clean slate).

## Prerequisites

- [Fly CLI](https://fly.io/docs/hands-on/install-flyctl/) and `fly auth login`
- An OpenRouter API key (`sk-or-…` from [openrouter.ai/keys](https://openrouter.ai/keys))

## One-time setup

1. Edit `fly.toml` and set `app = "your-unique-name"` (globally unique on Fly).

2. Create the app (if it does not exist):

   ```bash
   fly apps create your-unique-name
   ```

3. Create a volume for SQLite and uploads (region must match `primary_region` in `fly.toml`):

   ```bash
   fly volumes create open_webui_data --region iad --size 3
   ```

4. Set secrets:

   ```bash
   fly secrets set OPENAI_API_KEY="sk-or-..." \
     WEBUI_SECRET_KEY="$(openssl rand -hex 32)" \
     WEBUI_URL="https://your-unique-name.fly.dev"
   ```

   Optional but convenient: create the first admin in one step (disables open signup on first boot):

   ```bash
   fly secrets set WEBUI_ADMIN_EMAIL="you@example.com" WEBUI_ADMIN_PASSWORD='strong-password-here'
   ```

5. Deploy:

   ```bash
   cd deploy/open-webui && fly deploy
   ```

Open `https://your-unique-name.fly.dev`. In the UI, pick a model served by OpenRouter (IDs like `openai/gpt-4o`, `anthropic/claude-3.5-sonnet`, etc.).

## If the machine runs out of memory

The `fly.toml` env is tuned to avoid local embedding/STT models in RAM; if it still OOMs (large chats, document uploads, many users), scale up:

```bash
fly scale memory 2048
# or 4096 if needed
```

## When you *would* self-host a model

Only if you want **local / private inference** (no third-party API). That usually means **Ollama** or similar on a GPU-capable host, not this minimal Fly setup. For “just chat via API,” OpenRouter (or direct OpenAI) is enough.

## Notes

- `OPENAI_API_BASE_URL` points at OpenRouter’s OpenAI-compatible API; `OPENAI_API_KEY` is your OpenRouter key.
- After the first run, some settings are stored in the volume under `/app/backend/data` (see Open WebUI docs on `PersistentConfig`).
- Pin the image tag instead of `:main` in `fly.toml` if you want reproducible deploys.
