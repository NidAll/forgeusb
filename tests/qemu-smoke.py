#!/usr/bin/env python3
"""Boot ONLY a caller-selected regular image in isolated QEMU; no host disks/network.
Not a pass/fail boot oracle: preserve screenshot/serial evidence for inspection.
"""
import argparse,json,pathlib,shutil,socket,subprocess,time
p=argparse.ArgumentParser();p.add_argument('image',type=pathlib.Path);p.add_argument('--uefi',action='store_true');p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--seconds',type=int,default=50);p.add_argument('--serial-console',action='store_true');a=p.parse_args()
assert a.image.is_file() and not a.image.is_symlink() and not str(a.image.resolve()).startswith('/dev/')
assert 5<=a.seconds<=55
# Exclusive output directory provides fixture ownership and never reuses NVRAM.
a.output.mkdir(mode=0o700);root=a.output.resolve();image=a.image.resolve();qmp=root/'qmp.sock';serial=root/'serial.log'
cmd=['/usr/bin/qemu-system-x86_64','-machine','pc,accel=tcg','-m','384','-nic','none','-display','none','-serial',f'file:{serial}','-qmp',f'unix:{qmp},server=on,wait=off','-no-reboot','-drive',f'file={image},format=raw,if=none,id=media,readonly=on','-device','qemu-xhci','-device','usb-storage,drive=media,bootindex=1']
if a.uefi:
    vars=root/'OVMF_VARS.fd';shutil.copyfile('/usr/share/edk2/x64/OVMF_VARS.4m.fd',vars)
    cmd+=['-drive','if=pflash,format=raw,unit=0,readonly=on,file=/usr/share/edk2/x64/OVMF_CODE.4m.fd','-drive',f'if=pflash,format=raw,unit=1,file={vars}']
(root/'command.json').write_text(json.dumps(cmd,indent=2))
with (root/'qemu.log').open('w') as log:
    child=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT)
    try:
        deadline=time.monotonic()+8
        while not qmp.exists():
            assert child.poll() is None, 'QEMU exited before QMP'
            assert time.monotonic()<deadline,'QMP startup timeout'
            time.sleep(.05)
        s=socket.socket(socket.AF_UNIX);s.settimeout(3);s.connect(str(qmp));f=s.makefile('rwb',buffering=0);f.readline()
        def request(execute,arguments=None):
            obj={'execute':execute}
            if arguments is not None:obj['arguments']=arguments
            f.write(json.dumps(obj).encode()+b'\n')
            while True:
                line=json.loads(f.readline())
                if 'return' in line:return line
                if 'error' in line:raise RuntimeError(line)
        request('qmp_capabilities')
        start=time.monotonic();sent=False
        while time.monotonic()-start<a.seconds:
            if a.serial_console and not sent and serial.exists() and 'GNU GRUB' in serial.read_text(errors='replace'):
                request('human-monitor-command',{'command-line':'sendkey c'});time.sleep(.2)
                def type_line(line):
                    codes={'/':'slash','-':'minus','=':'equal',',':'comma',' ':'spc','S':'shift-s','\n':'ret'}
                    for ch in line:
                        key=codes.get(ch,ch)
                        request('human-monitor-command',{'command-line':f'sendkey {key} 10'})
                        time.sleep(.025)
                type_line('linux /boot/vmlinuz-virt modules=loop,squashfs,sd-mod,usb-storage console=ttyS0,115200\n')
                type_line('initrd /boot/initramfs-virt\n')
                type_line('boot\n');sent=True
            elif not a.serial_console and not sent and time.monotonic()-start>4:
                request('human-monitor-command',{'command-line':'sendkey ret'});sent=True
            time.sleep(.1)
        request('screendump',{'filename':str(root/'screen.ppm')})
        request('quit');child.wait(timeout=5)
        print(json.dumps({'qemu_exit':child.returncode,'uefi':a.uefi,'seconds':a.seconds,'evidence':str(root)}))
    finally:
        if child.poll() is None:child.terminate();child.wait(timeout=5)
