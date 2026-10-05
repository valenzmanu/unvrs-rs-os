#!/usr/bin/env python3
"""Exercise authenticated seat memory and ACP hot delivery through the actual binary."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time

ROOT=Path(__file__).resolve().parents[2]
BINARY=ROOT/'target/debug/unvrs'
HARNESS=r'''#!/usr/bin/env python3
import json,os,socket,subprocess,sys
from pathlib import Path
count=0
sessions=0
for line in sys.stdin:
    v=json.loads(line); method=v.get('method')
    if method=='initialize': result={'protocolVersion':1,'agentCapabilities':{}}
    elif method=='session/new':
        sessions+=1;result={'sessionId':'fixture-'+str(sessions)}
    elif method=='session/prompt':
        count+=1
        text=v['params']['prompt'][0]['text']
        replies={}
        for op in ['remember','recall']:
            r=subprocess.run([os.environ['EVAL_BINARY'],'ctl',op,'SeatBusUnique'],capture_output=True,text=True)
            replies[op]={'code':r.returncode,'out':r.stdout}
        for op in ['remember','forget','delete-note','scope','summary']:
            req={'op':op,'token':os.environ['UNVRS_TOKEN'],'title':'forged','pinned':True,'scope':{'area':'forged'},'id':'anything'}
            with socket.socket(socket.AF_UNIX) as s:
                s.connect(os.environ['UNVRS_SOCKET']);s.sendall((json.dumps(req)+'\n').encode());replies['deny-'+op]=json.loads(s.makefile().readline())
        Path('turn-'+str(count)+'.json').write_text(json.dumps({'text':text,'replies':replies,'session':v['params']['sessionId']}))
        print(json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'fixture','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'fixture reply'}}}}),flush=True)
        result={'stopReason':'end_turn'}
    else: result={}
    if 'id' in v: print(json.dumps({'jsonrpc':'2.0','id':v['id'],'result':result}),flush=True)
'''
with tempfile.TemporaryDirectory(prefix='unvrs-memory-bus-') as tmp:
    root=Path(tmp);home=root/'home';home.mkdir();universe=root/'universe';universe.mkdir()
    for directory in ['.claude','.codex']:
        (home/directory).mkdir();(home/directory/'sentinel').write_text('unchanged')
    harness=root/'fake-acp';harness.write_text(HARNESS);harness.chmod(0o700)
    env={**os.environ,'HOME':str(home),'UNVRS_ACP':str(harness),'EVAL_BINARY':str(BINARY)}
    master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0))
    process=subprocess.Popen([str(BINARY),'--fresh'],cwd=universe,env=env,stdin=slave,stdout=slave,stderr=slave);os.close(slave)
    def pump(seconds=.25):
        end=time.monotonic()+seconds
        while time.monotonic()<end:
            if select.select([master],[],[],.03)[0]:
                try: os.read(master,65536)
                except OSError: break
    def command(text): os.write(master,(text+' \r').encode());pump()
    def turn(n):
        path=universe/f'turn-{n}.json'
        deadline=time.monotonic()+8
        while not path.exists() and time.monotonic()<deadline:pump()
        assert path.exists(),'seat did not receive prompt'
        pump();return json.loads(path.read_text())
    try:
        pump(.6)
        command('/remember --pin EverywhereMarker')
        command('/scope area:alpha');command('/remember --pin AlphaMarker')
        command('/scope area:beta');command('/remember --pin BetaMarker')
        command('/scope area:alpha');command('first')
        first=turn(1)
        assert 'EverywhereMarker' in first['text'] and 'AlphaMarker' in first['text'] and 'BetaMarker' not in first['text']
        for op in ['remember','recall']:assert first['replies'][op]['code']==0,first['replies']
        for key,value in first['replies'].items():
            if key.startswith('deny-'):assert 'error' in value,key
        command('/scope area:beta');command('second');second=turn(2)
        assert first['session']!=second['session'],'scope switch must clear the old harness context'
        assert 'EverywhereMarker' in second['text'] and 'BetaMarker' in second['text'] and 'AlphaMarker' not in second['text']
        notes=list((universe/'.unvrs/memory/notes').glob('*.md'))
        seat_notes=[p.read_text() for p in notes if 'SeatBusUnique' in p.read_text()]
        assert len(seat_notes)==2 and all('pinned: false' in t and 'source: "pid:1"' in t for t in seat_notes)
        for directory in ['.claude','.codex']:
            assert list((home/directory).iterdir())==[home/directory/'sentinel']
            assert (home/directory/'sentinel').read_text()=='unchanged'
        os.write(master,b'\x1b[21~');pump();assert process.wait(timeout=10)==0
        print('PASS seat ctl authority, recall, ACP scope delivery and shared-home isolation')
    finally:
        if process.poll() is None:
            os.write(master,b'\x1b[21~');pump()
            try:process.wait(timeout=10)
            except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=5)
        os.close(master)
