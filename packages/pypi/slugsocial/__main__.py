import os
import platform
import shutil
import stat
import subprocess
import sys
import urllib.request
import urllib.error
from pathlib import Path

def env(name: str, default: str | None = None) -> str | None:
    v = os.getenv(name)
    if v is None:
        return default
    v = v.strip()
    return v if v else default


def _platform_triple() -> tuple[str, str]:
    sys_plat = sys.platform
    mach = platform.machine().lower()

    if sys_plat.startswith("darwin"):
        plat = "darwin"
    elif sys_plat.startswith("linux"):
        plat = "linux"
    elif sys_plat.startswith("win"):
        plat = "windows"
    else:
        raise SystemExit(f"unsupported platform: {sys_plat}")

    if mach in ("x86_64", "amd64"):
        arch = "x64"
    elif mach in ("arm64", "aarch64"):
        arch = "arm64"
    else:
        raise SystemExit(f"unsupported arch: {mach}")

    return plat, arch


def _default_cache_dir() -> Path:
    return Path.home() / ".cache" / "slugsocial"


def _download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".tmp")
    try:
        with urllib.request.urlopen(url) as r:
            if getattr(r, "status", None) != 200:
                raise SystemExit(f"download failed: {getattr(r, 'status', 'unknown')} {url}")
            with open(tmp, "wb") as f:
                shutil.copyfileobj(r, f)
    except urllib.error.HTTPError as e:
        raise SystemExit(f"download failed: {e.code} {url}") from e
    except urllib.error.URLError as e:
        raise SystemExit(f"download failed: {url} ({e})") from e

    # Basic corruption guard: reject HTML error pages or empty files.
    head = tmp.read_bytes()[:64]
    if not head or head.lstrip().startswith(b"<"):
        tmp.unlink(missing_ok=True)
        raise SystemExit(f"downloaded non-binary content from {url}")

    tmp.replace(dest)


def _ensure_binary() -> str:
    explicit = env("SLUGSOCIAL_BIN")
    if explicit:
        return explicit

    # Keep the shim thin; allow caller to configure.
    base = env("SLUGSOCIAL_RELEASE_BASE", "https://github.com/your-org/your-repo/releases/download")
    tag = env("SLUGSOCIAL_TAG", "v0.0.1")

    plat, arch = _platform_triple()
    ext = ".exe" if sys.platform.startswith("win") else ""
    asset = f"slugsocial-{plat}-{arch}{ext}"
    url = f"{base.rstrip('/')}/{tag}/{asset}"

    bin_path = _default_cache_dir() / f"{asset}-{tag}"
    if bin_path.exists():
        # If a previous attempt cached an HTML error page, drop it.
        try:
            head = bin_path.read_bytes()[:64]
            if not head or head.lstrip().startswith(b"<"):
                bin_path.unlink(missing_ok=True)
        except OSError:
            pass

    if not bin_path.exists():
        _download(url, bin_path)
        if not sys.platform.startswith("win"):
            st = os.stat(bin_path)
            os.chmod(bin_path, st.st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return str(bin_path)


def main() -> None:
    bin_path = _ensure_binary()
    # Exec the Rust CLI with original arguments.
    # (Use subprocess so behavior is consistent across platforms.)
    p = subprocess.run([bin_path, *sys.argv[1:]])
    raise SystemExit(p.returncode)


if __name__ == "__main__":
    main()


