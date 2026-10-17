#!/usr/bin/env bash
# Build as the desktop user; the installer elevates only the final file copy.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
exec /usr/bin/python3 "$root/packaging/install.py" "$@"
