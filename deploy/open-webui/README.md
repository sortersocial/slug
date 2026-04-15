# Open WebUI on Fly.io (minimal, OpenRouter)

You do **not** need to host your own model. [OpenRouter](https://openrouter.ai/) runs the LLMs; this Fly app only runs the Open WebUI shell.

### Disk vs RAM

The container image still includes ML libraries on **disk** (upstream wheels). These settings target **runtime memory**: chat and API proxy use remote inference, and `RAG_EMBEDDING_ENGINE=openai` plus `AUDIO_STT_ENGINE=openai` tell Open WebUI to call your configured OpenAI-compatible API for embeddings and speech-to-text instead of loading local SentenceTransformers / Whisper models into RAM (see Open WebUI [performance](https://docs.openwebui.com/troubleshooting/performance/) notes on embedding offload).

You may still see higher RSS if you use features that pull in other local code paths. For a fresh volume, the `[env]` values apply on first boot; if you already ran Open WebUI, stored admin settings can override some env vars ([PersistentConfig](https://docs.openwebui.com/reference/env-configuration/) — adjust in Admin or reset the volume for a clean slate).

## Prerequisites

- [Fly CLI](https://fly.io/docs/hands-on/install-flyctl/) and `fly auth login`
- An OpenRouter API key (`sk-or-…` from [openrouter.ai/keys](https://openrouter.ai/keys))

## One-time setup

Default in `fly.toml` is `app = "slug-open-webui"`. Change it if you want another **globally unique** name (names like `open-webui` are often already taken).

1. Create the Fly app (skip if it already exists):

   ```bash
   fly apps create slug-open-webui --org personal
   ```

2. Create a volume for SQLite and uploads (region must match `primary_region` in `fly.toml`, usually `iad`):

   ```bash
   fly volumes create open_webui_data -a slug-open-webui --region iad --size 3
   ```

3. Set secrets (add your OpenRouter key — required for chat through OpenRouter):

   ```bash
   fly secrets set -a slug-open-webui OPENAI_API_KEY="sk-or-..." \
     WEBUI_SECRET_KEY="$(openssl rand -hex 32)" \
     WEBUI_URL="https://slug-open-webui.fly.dev"
   ```

   You can also paste the API key in Open WebUI **Admin → Settings → Connections** if you prefer not to use a Fly secret for it.

### “No cookie auth credentials found” when sending a chat

On **HTTPS** (e.g. `*.fly.dev`), session cookies must use **Secure** + sensible **SameSite** settings, or streaming/WebSocket requests may not include your login cookie. `fly.toml` sets `WEBUI_*_COOKIE_SECURE` and `CORS_ALLOW_ORIGIN` for this. After a deploy, do a **hard refresh** or **clear site data** for the Open WebUI origin once so the browser picks up new cookies.

If it still fails, confirm **Admin → Settings → WebUI URL** is `https://slug-open-webui.fly.dev` (or your app name). Stale values in the SQLite DB can override env vars ([PersistentConfig](https://docs.openwebui.com/reference/env-configuration/)).

   Optional but convenient: create the first admin in one step (disables open signup on first boot):

   ```bash
   fly secrets set -a slug-open-webui WEBUI_ADMIN_EMAIL="you@example.com" WEBUI_ADMIN_PASSWORD='strong-password-here'
   ```

5. Deploy (from repo root):

   ```bash
   ./OPEN_WEBUI_DEPLOY.sh
   ```

   Equivalent: `fly deploy --config deploy/open-webui/fly.toml`

Open `https://slug-open-webui.fly.dev` (or your chosen app name). In the UI, pick a model served by OpenRouter (IDs like `openai/gpt-4o`, `anthropic/claude-3.5-sonnet`, etc.).

### HTTP 503 in the browser

Fly’s edge returns **503** when there is **no healthy machine** behind the app yet. Typical cases:

- **Right after `fly deploy` / secrets change / machine restart** — Open WebUI can take **1–2 minutes** to import dependencies and pass `/health`; the proxy logs show `could not find a good candidate at load balancing` until then.
- **While the process was OOM-killed** (undersized VM) — fixed here with `performance-2x` + swap.

**Check:** open `https://slug-open-webui.fly.dev/health` — you want `{"status":true}`. If that works but the main page was 503, wait and hard-refresh.

### `Error: unauthorized` right after login

Usually **not** a bad token. Fly app names are **global**; if `fly.toml` uses a name someone else already owns, deploy can fail with a vague `unauthorized`. Run `fly apps create your-name` — if you see **“Name has already been taken”**, pick another name and update `app` in `fly.toml`.

## VM size (important)

The stock Open WebUI image is not viable on Fly’s default **shared-cpu-1x (256 MB)** — the Python process gets OOM-killed during startup. `fly.toml` pins **`performance-2x`** (2 vCPU, 4 GB RAM) plus **`swap_size_mb = 1024`** so cold start and imports fit. That costs more than a tiny VM; to save money you’d need a slimmer image or a different host.

## If the machine runs out of memory

The `[env]` settings avoid loading local embedding/STT models when possible. If you still OOM (huge chats, many users), scale up in the Fly dashboard or `fly scale` / larger `[[vm]]` size.

## When you *would* self-host a model

Only if you want **local / private inference** (no third-party API). That usually means **Ollama** or similar on a GPU-capable host, not this minimal Fly setup. For “just chat via API,” OpenRouter (or direct OpenAI) is enough.

## Notes

- `OPENAI_API_BASE_URL` points at OpenRouter’s OpenAI-compatible API; `OPENAI_API_KEY` is your OpenRouter key.
- After the first run, some settings are stored in the volume under `/app/backend/data` (see Open WebUI docs on `PersistentConfig`).
- Pin the image tag instead of `:main` in `fly.toml` if you want reproducible deploys.
