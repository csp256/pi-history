# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte"]
# ///
import os,json,tempfile,subprocess,sys,pty,fcntl,termios,struct,select,time
from pathlib import Path
import pyte
binary=str(Path(sys.argv[1]).resolve())
cache=Path.home()/'.cache/pi-history/semantic/fastembed'
with tempfile.TemporaryDirectory() as temp:
    home=Path(temp);sessions=home/'sessions'/'project';sessions.mkdir(parents=True)
    auth='a1111111-1111-4111-8111-111111111111';food='b2222222-2222-4222-8222-222222222222';isovis='c3333333-3333-4333-8333-333333333333';lines='d4444444-4444-4444-8444-444444444444'
    for session,title,user,assistant in [(auth,'Persistent authentication','How can users remain signed in across browser restarts?','Store a refresh credential in a secure cookie. Persistent authentication restores login when the browser opens again.'),(food,'Roasting vegetables','How should I cook carrots and potatoes?','Roast vegetables in a hot oven with olive oil and salt until tender.'),(isovis,'Isovis implementation','hey isovis is pretty much just straight vanilla js, but with threejs, right?','Yes: Isovis is vanilla JavaScript using Three.js for rendering.'),(lines,'Counting source lines','What is the terminal tool we use for measuring source size?','Use this:\n```sh\nlines --depth 2 -d\n```')]:
        entries=[{'type':'session','id':session,'cwd':str(home),'timestamp':'2026-11-09T12:00:00Z'},{'type':'session_info','name':title},{'type':'message','id':'u1','message':{'role':'user','content':[{'type':'text','text':user}]}},{'type':'message','id':'a1','message':{'role':'assistant','content':[{'type':'text','text':assistant}]}}]
        if session==isovis:
            entries[2]['message']['content'].append({'type':'image','data':'AA==','mimeType':'image/png'})
            entries.insert(2,{'type':'message','id':'g1','message':{'role':'user','content':[{'type':'text','text':'Goal mode is active. Complete this goal fully:\n<goal_objective>Improve the visualization.</goal_objective>\n'+('Goal-mode rules: preserve objective, keep working, call goal_complete. '*80)+'\n<!-- pi-goal-prompt:fixture -->'}]}})
        (sessions/(session+'.jsonl')).write_text('\n'.join(json.dumps(entry) for entry in entries)+'\n')
    model=home/'.cache/pi-history/semantic/fastembed';model.parent.mkdir(parents=True);model.symlink_to(cache,target_is_directory=True)
    config=home/'.config/pi-history/config.toml';config.parent.mkdir(parents=True);config.write_text('[search]\nmode = "semantic"\n')
    env=dict(os.environ,HOME=str(home),PI_CODING_AGENT_SESSION_DIR=str(sessions.parent),RAYON_NUM_THREADS='2',TOKENIZERS_PARALLELISM='false',TERM='xterm-256color')
    query='keeping a web session logged in after closing the browser'
    for mode in ['--semantic','--hybrid']:
        result=subprocess.run([binary,'agent','search',query,mode,'--top','1'],env=env,text=True,capture_output=True,timeout=40)
        assert result.returncode==0,(result.stdout,result.stderr)
        assert 'Persistent authentication' in result.stdout and 'Roasting vegetables' not in result.stdout,(result.stdout,result.stderr)
        assert 'source=semantic' in result.stdout,result.stdout
        assert 'GetElementType' not in result.stderr,result.stderr
        print('PASS CLI',mode,flush=True)
    for query_text,title,evidence in [('question about isovis','Isovis implementation','vanilla'),('cli command for counting lines','Counting source lines','lines --depth 2 -d')]:
        result=subprocess.run([binary,'agent','search',query_text,'--semantic','--top','1'],env=env,text=True,capture_output=True,timeout=40)
        assert result.returncode==0,(result.stdout,result.stderr)
        assert title in result.stdout and evidence in result.stdout,(result.stdout,result.stderr)
        assert 'Goal-mode rules' not in result.stdout,result.stdout
        print('PASS natural-language retrieval:',query_text,flush=True)
    screen=pyte.Screen(150,30);stream=pyte.Stream(screen)
    master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,150,0,0))
    proc=subprocess.Popen([binary],env=env,stdin=slave,stdout=slave,stderr=slave);os.close(slave)
    raw=[]
    def read(seconds):
        end=time.monotonic()+seconds
        while time.monotonic()<end:
            if select.select([master],[],[],.05)[0]:
                try:data=os.read(master,65536).decode(errors='replace');raw.append(data);stream.feed(data)
                except OSError:break
        return '\n'.join(screen.display)
    try:
        read(4);os.write(master,query.encode());text=read(4)
        assert 'Persistent authentication' in text,text
        assert 'sem failed' not in text,text
        assert 'GetElementType' not in ''.join(raw),''.join(raw)
        print('PASS TUI startup/prewarm and live semantic query',flush=True)
        os.write(master,b'\x03');proc.wait(timeout=5)
    finally:
        if proc.poll() is None:proc.terminate();proc.wait(timeout=5)
        os.close(master)
