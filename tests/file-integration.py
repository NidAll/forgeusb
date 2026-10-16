#!/usr/bin/env python3
"""Unprivileged regression tests; every written path belongs to TemporaryDirectory.
Never enumerates loop devices or opens a physical device for writing.
"""
import argparse,hashlib,json,pathlib,subprocess,tempfile
p=argparse.ArgumentParser();p.add_argument('--cli',type=pathlib.Path,default=pathlib.Path('target/release/forgeusb'));a=p.parse_args();cli=str(a.cli.resolve())
results=[]
def run(args,good=True,input=None):
    r=subprocess.run(args,input=input,capture_output=True,text=True,timeout=60)
    assert (r.returncode==0)==good,(args,r.returncode,r.stdout[-1000:],r.stderr[-1000:])
    return r
with tempfile.TemporaryDirectory(prefix='forgeusb-owned-') as directory:
    root=pathlib.Path(directory)
    for scheme,filesystem in [('dos','fat32'),('gpt','fat32'),('dos','ntfs')]:
        image=root/f'{scheme}-{filesystem}.img';image.touch();image.open('r+b').truncate(96*1024*1024)
        kind='U' if scheme=='gpt' else ('c' if filesystem=='fat32' else '7')
        run(['/usr/bin/sfdisk',str(image)],input=f'label: {scheme}\nstart=2048, size=131072, type={kind}\n')
        if filesystem=='fat32':run(['/usr/bin/mkfs.fat','-F','32','--offset','2048',str(image),'65536'])
        else:
            fs=root/'ntfs-volume';fs.touch();fs.open('r+b').truncate(64*1024*1024)
            run(['/usr/bin/mkfs.ntfs','-F','-Q',str(fs)])
            with image.open('r+b') as out,fs.open('rb') as source:
                out.seek(1024*1024)
                while chunk:=source.read(1024*1024):out.write(chunk)
        output=root/f'{scheme}-{filesystem}-copy.img'
        inspected=json.loads(run([cli,'inspect',str(image),'--json']).stdout);assert inspected['bootable_disk']
        plan=json.loads(run([cli,'plan',str(image),'--device',str(output),'--test-file','--json']).stdout)
        assert not output.exists(),'planning created target'
        planfile=root/'plan.json';planfile.write_text(json.dumps(plan))
        run([cli,'write','--plan',str(planfile),'--confirm','incorrect'],good=False);assert not output.exists()
        run([cli,'write','--plan',str(planfile),'--confirm',plan['confirmation'],'--json'])
        assert hashlib.sha256(image.read_bytes()).digest()==hashlib.sha256(output.read_bytes()).digest()
        run([cli,'verify',str(image),'--device',str(output),'--test-file','--json'])
        run([cli,'write','--plan',str(planfile),'--confirm',plan['confirmation']],good=False)
        with output.open('r+b') as file:file.seek(12345);file.write(b'forged')
        run([cli,'verify',str(image),'--device',str(output),'--test-file'],good=False)
        results.append({'layout':scheme,'filesystem':filesystem,'read_only_plan':True,'write_readback':True,'overwrite_refused':True,'corruption_detected':True})
    # A real plain ISO must not silently select direct-write mode.
    tree=root/'tree';tree.mkdir();(tree/'hello').write_text('ordinary optical ISO')
    optical=root/'optical.iso'
    run(['/usr/bin/xorriso','-as','mkisofs','-o',str(optical),str(tree)])
    assert json.loads(run([cli,'inspect',str(optical),'--json']).stdout)['kind']=='optical_iso'
    run([cli,'plan',str(optical),'--device',str(root/'refused.img'),'--test-file'],good=False)
    results.append({'plain_optical_iso_refused':True})
print(json.dumps({'tests':results,'physical_writes':0,'loop_devices':0},indent=2))
