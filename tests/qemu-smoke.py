#!/usr/bin/env python3
"""Boot ONLY a caller-selected regular image in isolated QEMU; no host disks/network.
Preserve screenshot/serial evidence; --expect-serial adds an explicit boot oracle.
"""
import argparse,json,pathlib,shutil,socket,subprocess,time
from boot_tools import regular_image, ovmf_paths, tool, digest
p=argparse.ArgumentParser();p.add_argument('image',type=pathlib.Path);p.add_argument('--uefi',action='store_true');p.add_argument('--output',type=pathlib.Path,required=True);p.add_argument('--seconds',type=int,default=50);p.add_argument('--serial-console',action='store_true',help='request Alpine virt kernel serial console through Syslinux/GRUB');p.add_argument('--expect-serial',help='require this exact text in captured guest serial output');a=p.parse_args()
image=regular_image(a.image)
source_hash=digest(image)
if not 5<=a.seconds<=55:raise ValueError('--seconds must be 5..55')
# Exclusive output directory provides fixture ownership and never reuses NVRAM.
a.output.mkdir(mode=0o700);root=a.output.resolve();qmp=root/'qmp.sock';serial=root/'serial.log'
cmd=[tool('qemu-system-x86_64'),'-no-user-config','-machine','pc,accel=tcg','-m','384','-nic','none','-display','none','-serial',f'file:{serial}','-qmp',f'unix:{qmp},server=on,wait=off','-no-reboot','-blockdev',json.dumps({'driver':'raw','node-name':'media','read-only':True,'file':{'driver':'file','filename':str(image)}}),'-device','qemu-xhci','-device','usb-storage,drive=media,bootindex=1']
if a.uefi:
    code,template=ovmf_paths();vars=root/'OVMF_VARS.fd';shutil.copyfile(template,vars)
    cmd+=['-blockdev',json.dumps({'driver':'raw','node-name':'fwcode','read-only':True,'file':{'driver':'file','filename':str(code)}}),'-blockdev',json.dumps({'driver':'raw','node-name':'fwvars','file':{'driver':'file','filename':str(vars)}}),'-machine','pflash0=fwcode,pflash1=fwvars']
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
        def type_line(line):
            codes={'/':'slash','-':'minus','=':'equal',',':'comma',' ':'spc','S':'shift-s','\n':'ret'}
            for ch in line:
                key=codes.get(ch,ch)
                request('human-monitor-command',{'command-line':f'sendkey {key} 10'})
                time.sleep(.025)
        start=time.monotonic();sent=False
        while time.monotonic()-start<a.seconds:
            if a.serial_console and not sent and serial.exists() and 'GNU GRUB' in serial.read_text(errors='replace'):
                request('human-monitor-command',{'command-line':'sendkey c'});time.sleep(.2)
                type_line('linux /boot/vmlinuz-virt modules=loop,squashfs,sd-mod,usb-storage console=ttyS0,115200\n')
                type_line('initrd /boot/initramfs-virt\n')
                type_line('boot\n');sent=True
            elif a.serial_console and not a.uefi and not sent and serial.exists() and 'boot:' in serial.read_text(errors='replace'):
                type_line('virt console=ttyS0,115200\n');sent=True
            elif not a.serial_console and not sent and time.monotonic()-start>4:
                request('human-monitor-command',{'command-line':'sendkey ret'});sent=True
            time.sleep(.1)
        request('screendump',{'filename':str(root/'screen.ppm')})
        request('quit');child.wait(timeout=5)
        matched=a.expect_serial is None or a.expect_serial in serial.read_text(errors='replace')
        result={'qemu_exit':child.returncode,'uefi':a.uefi,'seconds':a.seconds,'evidence':str(root),'serial_expectation_met':matched,'image_sha256':source_hash,'image_unchanged':source_hash==digest(image),'physical_writes':0,'network':False,'secure_boot':'not tested'}
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        if not matched or not result['image_unchanged']:raise RuntimeError(f'Boot expectation failed; inspect {root}')
        print(json.dumps(result))
    finally:
        if child.poll() is None:child.terminate();child.wait(timeout=5)
