#!/usr/bin/env python3
"""Validate installer staging only; no elevation, installation, or devices."""
from pathlib import Path
import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent


def main():
    assert os.geteuid() != 0, 'run installer tests unprivileged'
    with tempfile.TemporaryDirectory(prefix='forgeusb-install-test-') as temporary:
        stage = Path(temporary) / 'stage'
        command = ['/usr/bin/bash', str(ROOT / 'packaging/install.sh'), '--no-build', '--stage', str(stage)]
        first = subprocess.run(command, capture_output=True, text=True)
        assert first.returncode == 0, first.stderr
        prefix = stage / 'usr'
        receipt = json.loads((prefix / 'share/forgeusb/install-receipt.json').read_text())
        assert len(receipt['files']) == 17, receipt['files']
        for relative, digest in receipt['files'].items():
            path = prefix / relative
            assert path.is_file() and not path.is_symlink()
            assert hashlib.sha256(path.read_bytes()).hexdigest() == digest
            assert stat.S_IMODE(path.stat().st_mode) == (0o755 if relative.startswith(('bin/', 'lib/')) else 0o644)
        for size in (16, 24, 32, 48, 64, 128, 256):
            data = (prefix / f'share/icons/hicolor/{size}x{size}/apps/forgeusb.png').read_bytes()
            assert data[:8] == b'\x89PNG\r\n\x1a\n'
            assert int.from_bytes(data[16:20], 'big') == size
            assert int.from_bytes(data[20:24], 'big') == size
        ET.parse(prefix / 'share/polkit-1/actions/io.forgeusb.write.policy')
        desktop = (prefix / 'share/applications/forgeusb.desktop').read_text()
        assert 'Icon=forgeusb\n' in desktop and 'StartupWMClass=forgeusb\n' in desktop
        again = subprocess.run(command, capture_output=True, text=True)
        assert again.returncode != 0 and 'existing staging trees are never overwritten' in again.stderr
        symlink = Path(temporary) / 'linked-stage'
        symlink.symlink_to(stage, target_is_directory=True)
        linked = subprocess.run([*command[:-1], str(symlink)], capture_output=True, text=True)
        assert linked.returncode != 0
        assert hashlib.sha256((prefix / 'bin/forgeusb-gui').read_bytes()).hexdigest() == receipt['files']['bin/forgeusb-gui']
        spec = importlib.util.spec_from_file_location('forgeusb_installer', ROOT / 'packaging/install.py')
        installer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(installer)
        user_prefix = Path(temporary) / 'user-install'
        payload = {path: source.read_bytes() for path, (source, _) in installer.files(False).items()}
        installer.apply(payload, user_prefix, False)
        changed = user_prefix / 'bin/forgeusb'
        original = changed.read_bytes()
        changed.write_bytes(b'changed fixture')
        try:
            installer.uninstall(user_prefix, False)
            raise AssertionError('uninstaller accepted changed contents')
        except RuntimeError as error:
            assert 'no files removed' in str(error)
        assert all((user_prefix / path).is_file() for path in payload)
        changed.write_bytes(original)
        installer.uninstall(user_prefix, False)
        assert all(not (user_prefix / path).exists() for path in payload)
        assert not (user_prefix / installer.RECEIPT).exists()
        print(json.dumps({'ok': True, 'staged_files': len(receipt['files']), 'png_sizes': 7,
                          'existing_destination_refused': True, 'symlink_destination_refused': True,
                          'changed_file_uninstall_refused': True, 'receipt_matched_uninstall': True,
                          'elevation': False, 'physical_writes': 0}))


if __name__ == '__main__':
    main()
