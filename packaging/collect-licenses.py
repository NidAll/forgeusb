#!/usr/bin/env python3
"""Collect shipped crate notices for the pinned Linux target; no network requests."""
import json,pathlib,subprocess
root=pathlib.Path(__file__).resolve().parent.parent
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--offline','--filter-platform','x86_64-unknown-linux-gnu','--format-version','1'],cwd=root))
packages=sorted(metadata['packages'],key=lambda p:(p['name'],p['version']))
iced=next(pathlib.Path(p['manifest_path']).parent/'LICENSE' for p in packages if p['name']=='iced')
text=['# Linux x86_64 Rust dependency license/notice texts\n# Collected from exact Cargo.lock, including test dependencies.\n'];missing=[]
for package in packages:
    if package['source'] is None:continue
    path=pathlib.Path(package['manifest_path']).parent
    files=sorted(p for p in path.rglob('*') if p.is_file() and any(p.name.upper().startswith(n) for n in ['LICENSE','COPYING','COPYRIGHT','NOTICE']) and p.stat().st_size<200000)
    text.append('\n'+'='*72+f"\n{package['name']} {package['version']} — {package.get('license')}\n")
    if not files and package['name'].startswith('iced_'):
        text.append('Iced workspace subcrate; shared repository MIT notice retained from locked iced umbrella crate.\n');files=[iced]
    if not files:
        missing.append(package['name'])
        text.append(f"No license file was shipped in this crate. Declared license: {package.get('license')}. Repository: {package.get('repository')}. Authors: {', '.join(package['authors'])}. See README and manifest attribution; review this upstream packaging omission before public binary redistribution.\n")
    for file in files:text.append(f'\n--- {file.name} ---\n'+file.read_text(errors='replace')+'\n')
(root/'docs/third-party-licenses.txt').write_text(''.join(text))
print(json.dumps({'packages':len(packages),'upstream_notice_files_missing':missing}))
