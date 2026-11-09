# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte"]
# ///
"""macOS/Linux PTY smoke test with isolated history/config and no model calls."""
import os,pty,subprocess,tempfile,json,fcntl,termios,struct,select,time,re,sys,pyte
from pathlib import Path
binary=str(Path(sys.argv[1] if len(sys.argv)>1 else "target/debug/pi-history").resolve())
with tempfile.TemporaryDirectory() as home:
    home=Path(home); path=home/'session.jsonl'
    def append(entry):
        with path.open('a') as f:f.write(json.dumps(entry)+'\n')
    append({'type':'session','id':'ansi-test','cwd':str(home)})
    for i in range(35):append({'type':'user','message':{'role':'user','content':f'EARLIER_{i:03}'}})
    append({'type':'assistant','message':{'role':'assistant','content':[{'type':'text','text':'**Normal markdown stays**'},{'type':'thinking','thinking':'\x1b[31mANSI_RED\x1b[39m normal\n\x1b[38;2;1;2;3;48;5;21;4mANSI_RGB\x1b[0m\n\x1b]52;c;CLIPBOARD_SECRET\x07\x1b[2JTHINK_LAST'}]}})
    def run(args, expected=True, exercise=False):
        screen=pyte.Screen(180,25); stream=pyte.Stream(screen)
        master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',25,180,0,0))
        proc=subprocess.Popen([binary,*args,str(path)],stdin=slave,stdout=slave,stderr=slave,env=dict(os.environ,TERM='xterm-256color',HOME=str(home),RAYON_NUM_THREADS='1'));os.close(slave)
        def read(seconds=.3):
            end=time.monotonic()+seconds
            while time.monotonic()<end:
                if select.select([master],[],[],.05)[0]:
                    try:stream.feed(os.read(master,65536).decode(errors='replace'))
                    except OSError:break
            return '\n'.join(screen.display)
        def bottom(text):
            match=re.search(r'\[\s*(\d+)\s*/\s*(\d+)\s*\]',text);assert match,text
            first,total=map(int,match.groups());assert first-1==max(total-22,0),(first,total,text)
        def cell(marker):
            for y,line in enumerate(screen.display):
                if marker in line:return screen.buffer[y][line.index(marker)]
            raise AssertionError(marker+' missing\n'+'\n'.join(screen.display))
        try:
            text=read(1)
            assert ('ANSI·on' if expected else 'ANSI·off') in text,text
            if expected:
                assert 'ANSI_RED' in text and 'ANSI_RGB' in text and 'THINK_LAST' in text,text
                assert '[39m' not in text and 'CLIPBOARD_SECRET' not in text,text
                assert cell('ANSI_RED').fg in ('red','cd0000'),cell('ANSI_RED')
                assert cell('ANSI_RGB').fg=='010203',cell('ANSI_RGB')
                assert cell('ANSI_RGB').bg=='0000ff',cell('ANSI_RGB')
                assert cell('ANSI_RGB').underscore
                bottom(text)
            if exercise:
                for _ in range(2):
                    os.write(master,b'A');text=read(.4);bottom(text)
                assert 'ANSI·on' in text and '[39m' not in text,text
                append({'type':'user','message':{'role':'user','content':'AFTER_ANSI_PLAIN'}})
                text=read(1.8);assert 'AFTER_ANSI_PLAIN' in text,text;bottom(text)
                assert cell('AFTER_ANSI_PLAIN').fg not in ('red','cd0000')
                os.write(master,b'g');read(.3);os.write(master,b'A');text=read(.4);assert 'EARLIER_000' in text,text
            os.write(master,b'q');proc.wait(timeout=3)
        finally:
            if proc.poll() is None:proc.terminate();proc.wait(timeout=3)
            os.close(master)
    run(['--show-thinking','--render-ansi'],exercise=True)
    config=home/'.config/pi-history/config.toml';config.parent.mkdir(parents=True);config.write_text('[display]\nrender_ansi = true\n')
    run(['--show-thinking'])
    run(['--show-thinking','--no-render-ansi'],expected=False)
    print('PASS: ANSI foreground/background/RGB/underline; reset cleanup; CSI/OSC filtering; toggle/latest anchoring; live reload; scrolled-away position; config and CLI precedence.')
