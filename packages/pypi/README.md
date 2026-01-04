# `uvx slugsocial`

Ultra-thin shim that **downloads** (once) and **execs** the Rust `slugsocial` binary.

## Usage

```bash
# Option A: point at an existing local build
export SLUGSOCIAL_BIN="/path/to/slugsocial"

uvx slugsocial healthz
```

```bash
# Option B: download from GitHub Releases (recommended)
export SLUGSOCIAL_RELEASE_BASE="https://github.com/<org>/<repo>/releases/download"
export SLUGSOCIAL_TAG="v0.0.1"

uvx slugsocial --help
```

## Publish checklist (PyPI)

- Bump version in `packages/pypi/pyproject.toml`.
- Publish from `packages/pypi`:

```bash
python -m build
twine upload dist/*
```


