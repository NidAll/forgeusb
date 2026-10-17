#!/usr/bin/env python3
"""Install ForgeUSB from a reviewed source checkout; never accesses storage devices."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import pwd
import shutil
import stat
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent
SIZES = (16, 24, 32, 48, 64, 128, 256)
RECEIPT = Path('share/forgeusb/install-receipt.json')
LIMIT = 128 * 1024 * 1024


def files(system: bool) -> dict[Path, tuple[Path, int]]:
    result = {
        Path('bin/forgeusb'): (ROOT / 'target/release/forgeusb', 0o755),
        Path('bin/forgeusb-gui'): (ROOT / 'target/release/forgeusb-gui', 0o755),
        Path('share/applications/forgeusb.desktop'): (ROOT / 'packaging/forgeusb.desktop', 0o644),
        Path('share/icons/hicolor/scalable/apps/forgeusb.svg'): (ROOT / 'packaging/icons/forgeusb.svg', 0o644),
    }
    for size in SIZES:
        result[Path(f'share/icons/hicolor/{size}x{size}/apps/forgeusb.png')] = (
            ROOT / f'packaging/icons/forgeusb-{size}.png', 0o644)
    for name, source in [('LICENSE', 'LICENSE'), ('THIRD_PARTY_NOTICES.md', 'THIRD_PARTY_NOTICES.md'),
                         ('third-party-licenses.txt', 'docs/third-party-licenses.txt'),
                         ('dependency-licenses.json', 'docs/dependency-licenses.json')]:
        result[Path('share/licenses/forgeusb') / name] = (ROOT / source, 0o644)
    if system:
        result[Path('lib/forgeusb/forgeusb-helper')] = (ROOT / 'target/release/forgeusb-helper', 0o755)
        result[Path('share/polkit-1/actions/io.forgeusb.write.policy')] = (
            ROOT / 'packaging/polkit/io.forgeusb.write.policy', 0o644)
    return result


def run(argv: list[str]) -> None:
    subprocess.run(argv, check=True)


def read_regular(path: Path) -> bytes:
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= LIMIT:
            raise RuntimeError(f'Not a bounded regular installer input: {path}')
        data = stream.read(LIMIT + 1)
        if len(data) != info.st_size:
            raise RuntimeError(f'Installer input changed: {path}')
        return data


def protected_directory(directory: Path) -> None:
    """Every system destination ancestor must be a trusted real directory."""
    if directory != Path('/'):
        protected_directory(directory.parent)
    try:
        info = directory.lstat()
    except FileNotFoundError:
        directory.mkdir(mode=0o755)
        info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
        raise RuntimeError(f'Refusing untrusted system directory: {directory}')


def replace_file(path: Path, data: bytes, mode: int, system: bool) -> None:
    if system:
        protected_directory(path.parent)
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise RuntimeError(f'Refusing nonregular destination: {path}')
    descriptor, temporary = tempfile.mkstemp(prefix='.forgeusb-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(data)
            os.fchmod(stream.fileno(), mode)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def apply(payload: dict[Path, bytes], prefix: Path, system: bool) -> None:
    entries = {path: value for path, value in files(True).items() if path in payload}
    # Fail before any mutation on unsafe destinations or package-owned files.
    for relative in [*entries, RECEIPT]:
        destination = prefix / relative
        if system:
            protected_directory(destination.parent)
            if destination.exists() and Path('/usr/bin/pacman').exists():
                owned = subprocess.run(['/usr/bin/pacman', '-Qqo', str(destination)],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                if owned.returncode == 0:
                    raise RuntimeError(f'Package-managed file; upgrade with the package manager: {destination}')
        if destination.is_symlink() or (destination.exists() and not destination.is_file()):
            raise RuntimeError(f'Refusing nonregular destination: {destination}')
    for relative, (_, mode) in entries.items():
        replace_file(prefix / relative, payload[relative], mode, system)
    receipt = {'schema': 1, 'prefix': str(prefix), 'system': system,
               'files': {str(path): hashlib.sha256(data).hexdigest() for path, data in payload.items()}}
    replace_file(prefix / RECEIPT, (json.dumps(receipt, indent=2) + '\n').encode(), 0o644, system)
    # Refresh caches when available, without fetching/installing dependencies.
    for executable, arguments in [('/usr/bin/update-desktop-database', [str(prefix / 'share/applications')]),
                                   ('/usr/bin/gtk-update-icon-cache', ['--force', '--ignore-theme-index', str(prefix / 'share/icons/hicolor')])]:
        if Path(executable).is_file():
            result = subprocess.run([executable, *arguments], check=False)
            if result.returncode:
                print(f'Warning: desktop cache refresh failed: {executable}', file=sys.stderr)
    print(f'Installed ForgeUSB into {prefix}; GUI remains unprivileged.')


def uninstall(prefix: Path, system: bool) -> None:
    receipt_path = prefix / RECEIPT
    if system:
        protected_directory(receipt_path.parent)
    receipt = json.loads(read_regular(receipt_path))
    expected = receipt.get('files', {})
    if set(expected) != {str(path) for path in files(system)} or receipt.get('prefix') != str(prefix):
        raise RuntimeError('Unrecognized installation receipt; no files removed')
    for relative, digest in expected.items():
        destination = prefix / relative
        if system:
            protected_directory(destination.parent)
        if hashlib.sha256(read_regular(destination)).hexdigest() != digest:
            raise RuntimeError(f'Installed file changed; no files removed: {destination}')
    # Remove authorization policy first. Never recursively remove directories.
    for relative in sorted(expected, key=lambda name: 'polkit-1' not in name):
        (prefix / relative).unlink()
    receipt_path.unlink()
    print(f'Uninstalled the receipt-matched ForgeUSB files from {prefix}.')


def elevated_apply(directory: Path) -> None:
    if os.geteuid() != 0:
        raise RuntimeError('Internal system copy requires administrator authorization')
    uid = int(os.environ.get('PKEXEC_UID', os.environ.get('SUDO_UID', '0')))
    if uid <= 0:
        raise RuntimeError('Run install.sh as your desktop user, not through sudo')
    account = pwd.getpwuid(uid)
    old_groups = os.getgroups()
    # Read staging inputs with the caller's credentials, never root read access.
    # Snapshot all bytes before restoring privileges and touching /usr.
    try:
        os.initgroups(account.pw_name, account.pw_gid)
        os.setegid(account.pw_gid)
        os.seteuid(uid)
        expected = json.loads(read_regular(directory / 'checksums.json'))
        payload = {path: read_regular(directory / path) for path in files(True)}
        actual = {str(path): hashlib.sha256(data).hexdigest() for path, data in payload.items()}
        if expected != actual:
            raise RuntimeError('Installer staging checksum mismatch; rebuild and retry')
    finally:
        os.seteuid(0)
        os.setegid(0)
        os.setgroups(old_groups)
    apply(payload, Path('/usr'), True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--system', action='store_true', help='install to /usr with Polkit helper (default)')
    mode.add_argument('--user', action='store_true', help='install desktop/CLI in ~/.local, without privileged helper')
    mode.add_argument('--stage', type=Path, help='stage /usr layout in a new directory, without elevation')
    parser.add_argument('--no-build', action='store_true', help='install already built target/release binaries')
    parser.add_argument('--apply-system', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--uninstall', action='store_true', help='remove only unmodified files recorded by this installer')
    parser.add_argument('--remove-system', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.remove_system:
        if os.geteuid() != 0 or int(os.environ.get('PKEXEC_UID', os.environ.get('SUDO_UID', '0'))) <= 0:
            raise RuntimeError('Use install.sh --system --uninstall as your desktop user')
        uninstall(Path('/usr'), True)
        return
    if args.apply_system:
        elevated_apply(args.apply_system)
        return
    if os.geteuid() == 0:
        raise RuntimeError('Do not build/run this installer as root; it elevates only the file copy')
    system = not args.user
    if args.uninstall:
        if args.stage:
            raise RuntimeError('--uninstall cannot be combined with --stage')
        if args.user:
            uninstall(Path.home() / '.local', False)
        else:
            with tempfile.TemporaryDirectory(prefix='forgeusb-uninstall-') as temporary:
                worker = Path(temporary) / 'install.py'
                worker.write_bytes(Path(__file__).read_bytes())
                elevation = '/usr/bin/pkexec' if Path('/usr/bin/pkexec').is_file() else '/usr/bin/sudo'
                run([elevation, '/usr/bin/python3', str(worker), '--remove-system'])
        return
    if not args.no_build:
        subprocess.run([sys.executable, str(ROOT / 'packaging/build.py'),
                        'build', '--release', '--locked'], cwd=ROOT, check=True)
    payload = {path: read_regular(source) for path, (source, _) in files(system).items()}
    for path in ['bin/forgeusb', 'bin/forgeusb-gui'] + (['lib/forgeusb/forgeusb-helper'] if system else []):
        if not payload[Path(path)].startswith(b'\x7fELF'):
            raise RuntimeError(f'Expected a Linux ELF executable: {path}')
    ET.fromstring(payload[Path('share/icons/hicolor/scalable/apps/forgeusb.svg')])
    if system:
        ET.fromstring(payload[Path('share/polkit-1/actions/io.forgeusb.write.policy')])
    if args.user:
        prefix = Path.home() / '.local'
        desktop = payload[Path('share/applications/forgeusb.desktop')].decode()
        escaped = str(prefix / 'bin/forgeusb-gui').replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$').replace('%', '%%')
        payload[Path('share/applications/forgeusb.desktop')] = desktop.replace('Exec=forgeusb-gui', f'Exec="{escaped}"').encode()
        apply(payload, prefix, False)
        print('Physical writes still require a separate --system installation.')
    elif args.stage:
        prefix = args.stage.absolute()
        if prefix.exists():
            raise RuntimeError('--stage requires a new directory; existing staging trees are never overwritten')
        apply(payload, prefix / 'usr', False)
        print('Staging only: no system helper or policy was activated.')
    else:
        with tempfile.TemporaryDirectory(prefix='forgeusb-install-') as temporary:
            directory = Path(temporary)
            for relative, data in payload.items():
                replace_file(directory / relative, data, 0o644, False)
            (directory / 'checksums.json').write_text(json.dumps({str(path): hashlib.sha256(data).hexdigest() for path, data in payload.items()}))
            worker = directory / 'install.py'
            worker.write_bytes(Path(__file__).read_bytes())
            # The fixed /usr allowlist is independent of the checkout location.
            elevation = '/usr/bin/pkexec' if Path('/usr/bin/pkexec').is_file() else '/usr/bin/sudo'
            print('Administrator authentication is required to install the fixed /usr files.', flush=True)
            run([elevation, '/usr/bin/python3', str(worker), '--apply-system', str(directory)])
    if not args.stage and Path('/usr/bin/kbuildsycoca6').is_file():
        subprocess.run(['/usr/bin/kbuildsycoca6', '--noincremental'], check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError, ET.ParseError) as error:
        print(f'ForgeUSB installation failed: {error}', file=sys.stderr)
        sys.exit(1)
