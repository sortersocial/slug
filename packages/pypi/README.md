# `uvx slugsocial`

Ultra-thin shim that **execs the Rust `slugsocial` binary bundled in platform wheels**.

## Usage

Install/run:

```bash
uvx slugsocial --help
```

## Publish checklist (PyPI)

- Bump version in `packages/pypi/pyproject.toml`.
- Publish from `packages/pypi`:

```bash
python -m build
twine upload dist/*
```


