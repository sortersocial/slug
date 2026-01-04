import platform
import stat
import subprocess
import sys
from pathlib import Path

def env(name: str, default: str | None = None) -> str | None:
    # Kept for backwards compatibility with earlier shims; no longer used for downloading.
    import os
    v = os.getenv(name)
    if v is None:
        return default
    v = v.strip()
    return v if v else default


def _bundled_binary() -> Path:
    ext = ".exe" if sys.platform.startswith("win") else ""
    return Path(__file__).resolve().parent / "bin" / f"slugsocial{ext}"


def main() -> None:
    bin_path = _bundled_binary()
    if not bin_path.exists():
        plat = sys.platform
        mach = platform.machine().lower()
        raise SystemExit(
            "slugsocial binary not found in this installation.\n"
            f"platform={plat} arch={mach}\n"
            "This should be provided by platform wheels. If you're running from source, "
            "build the Rust CLI and copy it into packages/pypi/slugsocial/bin/."
        )
    if not sys.platform.startswith("win"):
        st = bin_path.stat()
        bin_path.chmod(st.st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    # Exec the Rust CLI with original arguments.
    # (Use subprocess so behavior is consistent across platforms.)
    p = subprocess.run([bin_path, *sys.argv[1:]])
    raise SystemExit(p.returncode)


if __name__ == "__main__":
    main()


