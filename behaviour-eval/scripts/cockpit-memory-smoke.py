#!/usr/bin/env python3
"""A11 plus captain memory paths over a real PTY; B1–B10 remain captain checks."""
import codecs
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]
binary = ROOT / 'target/debug/unvrs'
ansi = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[()][B0]|\x1b[=>]')
with tempfile.TemporaryDirectory(prefix='unvrs-memory-cockpit-') as tmp:
    root = Path(tmp)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 44, 140, 0, 0))
    process = subprocess.Popen([str(binary), '--demo', '--fresh'], cwd=root, stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)
    decoder = codecs.getincrementaldecoder('utf8')(errors='replace')
    tape = ''
    def pump(seconds=.25):
        global tape
        until = time.monotonic()+seconds
        while time.monotonic()<until:
            if select.select([master],[],[],.03)[0]:
                try: tape += decoder.decode(os.read(master,65536))
                except OSError: break
    def send(text):
        os.write(master,text.encode()); pump()
    def command(text):
        send(text+' '); send('\r')
    def notes():
        return list((root/'.unvrs/memory/notes').glob('*.md'))
    def checkpoint(label):
        assert process.poll() is None, 'cockpit exited: '+ansi.sub('',tape[-2000:])
        print('PASS '+label,flush=True)
    try:
        pump(.6)
        send('/')
        text=ansi.sub('',tape)
        assert '/remember' in text and '/more' in text and '/forget' not in text
        send('\x1b');command('/more');assert '/forget' in ansi.sub('',tape)
        send('\x1b');command('/onboard')
        for scene,title in enumerate(['THE FLIGHT IS A UNIVERSE','CAPTAIN HOLDS CONTEXT','TOOLS ABOARD']):
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 44, 140+scene%2, 0, 0));pump(.3)
            assert title in ansi.sub('',tape),title
            send('\r')
        command('/remember --pin --open first-memory | shared fact')
        assert len(notes())==1
        first=notes()[0]
        assert 'pinned: true' in first.read_text()
        checkpoint('onboard completes and cockpit accepts remember')
        command('/scope area:sales')
        command('/remember --pin --open sales-memory | customer fact')
        assert len(notes())==2
        command('/recall shared')
        assert first.stem in ansi.sub('',tape)
        command('/developer')
        assert 'SYSTEM' in ansi.sub('',tape)
        command('/developer')
        command('/forget '+first.stem)
        assert 'status: retired' in first.read_text()
        command('/delete-note '+first.stem)
        assert not first.exists()
        second=notes()[0]
        command('/admit '+second.stem)
        assert 'outcome' in ansi.sub('',tape).lower()
        send('A customer brief exists | The brief has the agreed price\r')
        assert 'settled:' in second.read_text()
        assert len(list((root/'.unvrs/missions').glob('*.md')))==1
        checkpoint('scope, recall, retire, delete and admit from note work')
        command('/onboard');send('\x1b')
        command('/remember after-escape')
        assert len(notes())==2
        command('/comms');send('hello after onboard\r');pump(2)
        checkpoint('Escape and completion return to a working input')
        send('\x1b[21~');process.wait(timeout=10)
        assert process.returncode==0
        saved=json.loads((root/'.unvrs/session-demo.json').read_text())
        assert 'hello after onboard' in saved['crew'][0]['transcript']
        assert 'SYSTEM /' not in saved['crew'][0]['transcript']
        print('PASS system events are separate from narration; F10 exits 0',flush=True)
    finally:
        if process.poll() is None:
            os.write(master,b'\x1b[21~')
            try: process.wait(timeout=10)
            except subprocess.TimeoutExpired: process.terminate();process.wait(timeout=5)
        os.close(master)
