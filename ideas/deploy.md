# Deploying the server (`slug.social`)

CLI releases are tag-driven (see `.github/workflows/release.yml`).

The **server/site** is deployed to Fly.io from `main`.

## Automatic deploys (GitHub Actions)

Workflow: `.github/workflows/deploy.yml`

Triggers:
- push to `main` that touches:
  - `server/**`
  - `Dockerfile`
  - `fly.toml`
- or manual run via **Actions → Deploy server (Fly.io) → Run workflow**

Required secret:
- **`FLY_API_TOKEN`** (repo secret is fine; environment secret also fine)

Create token:

```bash
fly auth token
```

Then add it in GitHub:
- Repo → **Settings → Secrets and variables → Actions → New repository secret**
- Name: `FLY_API_TOKEN`

## Manual deploy (local)

```bash
fly deploy
```

App name is set in `fly.toml` (`app = "slugsocial"`).


