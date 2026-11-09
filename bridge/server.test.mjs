import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, readdirSync, rmSync, statSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connect } from 'node:net';
import { startBridge } from './server.ts';
import extension from './index.ts';

async function fixture(run) {
  const directory = mkdtempSync(join(tmpdir(), 'pi-history-bridge-'));
  const sessionFile = join(directory, 'session.jsonl');
  writeFileSync(sessionFile, '{}\n');
  let state = {sessionFile, sessionId:'session-one', name:'Test session', busy:false};
  const received = [];
  const server = await startBridge({directory, getSession:()=>state, send:(message, delivery)=>received.push({message,delivery})});
  const record = JSON.parse(readFileSync(server.registryFile,'utf8'));
  const request = (body)=>wire({...record,...body});
  try { await run({directory,sessionFile,received,server,record,request,setState:(next)=>state={...state,...next}}); }
  finally { await server.close(); rmSync(directory,{recursive:true,force:true}); }
}
function wire(request) {
  return new Promise((resolve,reject)=>{
    const socket=connect(request.port,'127.0.0.1'); let text='';
    socket.setTimeout(4000,()=>socket.destroy(new Error('timeout')));
    socket.on('connect',()=>socket.write(JSON.stringify(request)+'\n'));
    socket.on('data',(chunk)=>text+=chunk);
    socket.on('error',reject);
    socket.on('end',()=>{try {resolve(JSON.parse(text));} catch(error){reject(error);}});
  });
}
const send = (requestId='request-one',message='Hello',delivery='steer')=>({type:'send',requestId,message,delivery});

test('private registry, idle prompt, busy steering and follow-up acknowledgements', async()=>fixture(async({request,setState,received,server,directory})=>{
  if(process.platform!=='win32') {
    assert.equal(statSync(directory).mode&0o777,0o700);
    assert.equal(statSync(server.registryFile).mode&0o777,0o600);
  }
  assert.equal((await request({type:'status'})).busy,false);
  assert.equal((await request(send())).delivery,'prompt');
  setState({busy:true});
  assert.equal((await request(send('two','Busy message'))).delivery,'steer');
  assert.equal((await request(send('three','Follow later','followUp'))).delivery,'followUp');
  assert.deepEqual(received.map(x=>x.delivery),['steer','steer','followUp']);
}));

test('rejects bad authentication, wrong session/protocol, blank and oversized messages',async()=>fixture(async({request,received})=>{
  for(const body of [
    {...send(),token:'bad'}, {...send(),version:2}, {...send(),sessionId:'another'},
    {...send(),sessionFile:'/wrong/session.jsonl'}, send('id','  '), send('id','x'.repeat(32769)),
    send('id','nul\0text'),send('id','hello','invalid'),send('bad id'),
  ]) assert.equal((await request(body)).ok,false);
  assert.equal(received.length,0);
}));

test('duplicate request IDs deliver once and cannot change text or delivery',async()=>fixture(async({request,received})=>{
  const replies=await Promise.all([request(send()),request(send()),request(send())]);
  assert(replies.every(x=>x.ok)); assert.equal(received.length,1);
  assert.equal((await request(send('request-one','Different'))).ok,false);
  assert.equal((await request(send('request-one','Hello','followUp'))).ok,false);
  assert.equal(received.length,1);
}));

test('live identity checks prevent delivery after session replacement',async()=>fixture(async({request,setState,received})=>{
  setState({sessionId:'new-session'});
  assert.equal((await request(send())).ok,false);
  assert.equal(received.length,0);
}));

test('shutdown is idempotent and removes the discovery record',async()=>fixture(async({server,directory})=>{
  await server.close(); await server.close();
  assert(!readdirSync(directory).some(x=>x.endsWith('.json')));
}));

test('refuses a non-private registry directory',async()=>{
  if(process.platform==='win32')return;
  const directory=mkdtempSync(join(tmpdir(),'pi-history-insecure-'));
  chmodSync(directory,0o755);
  try {await assert.rejects(startBridge({directory,getSession:()=>({sessionFile:'unused',sessionId:'id',busy:false}),send:()=>{}}),/private/);}
  finally{rmSync(directory,{recursive:true,force:true});}
});

test('Pi adapter uses the user-message API without changing Pi settings',async()=>{
  const directory=mkdtempSync(join(tmpdir(),'pi-history-adapter-'));
  const sessionFile=join(directory,'session.jsonl');writeFileSync(sessionFile,'{}\n');
  const previous=process.env.PI_HISTORY_BRIDGE_DIR;process.env.PI_HISTORY_BRIDGE_DIR=directory;
  const handlers=new Map();const calls=[];
  extension({on:(event,handler)=>handlers.set(event,handler),registerCommand:()=>{},sendUserMessage:(...args)=>calls.push(args)});
  const ctx={mode:'rpc',isIdle:()=>false,sessionManager:{getSessionFile:()=>sessionFile,getSessionId:()=> 'id',getSessionName:()=> 'Name'},ui:{notify:()=>{}}};
  try{
    await handlers.get('session_start')({},ctx);
    const path=join(directory,readdirSync(directory).find(x=>x!=='session.jsonl'));
    const record=JSON.parse(readFileSync(path,'utf8'));
    assert.equal((await wire({...record,...send()})).ok,true);
    assert.deepEqual(calls,[['Hello',{deliverAs:'steer',expandPromptTemplates:false}]]);
    await handlers.get('session_shutdown')({},ctx);
    assert(!readdirSync(directory).some(x=>x!=='session.jsonl'));
  }finally{
    await handlers.get('session_shutdown')({},ctx);
    if(previous===undefined)delete process.env.PI_HISTORY_BRIDGE_DIR;else process.env.PI_HISTORY_BRIDGE_DIR=previous;
    rmSync(directory,{recursive:true,force:true});
  }
});

test('new session can register before its first persisted message without creating history',async()=>{
  const directory=mkdtempSync(join(tmpdir(),'pi-history-new-'));
  const sessionFile=join(directory,'future.jsonl');
  const server=await startBridge({directory,getSession:()=>({sessionFile,sessionId:'new',busy:false}),send:()=>{}});
  try{
    assert(!readdirSync(directory).includes('future.jsonl'));
    writeFileSync(sessionFile,'{}\n');
    const record=JSON.parse(readFileSync(server.registryFile,'utf8'));
    assert.equal((await wire({...record,type:'status'})).ok,true);
  }finally{await server.close();rmSync(directory,{recursive:true,force:true});}
});

test('overlapping lifecycle events never leave orphan listeners or discovery records',async()=>{
  const directory=mkdtempSync(join(tmpdir(),'pi-history-lifecycle-'));
  const sessionFile=join(directory,'session.jsonl');writeFileSync(sessionFile,'{}\n');
  const previous=process.env.PI_HISTORY_BRIDGE_DIR;process.env.PI_HISTORY_BRIDGE_DIR=directory;
  const handlers=new Map();
  extension({on:(event,handler)=>handlers.set(event,handler),registerCommand:()=>{},sendUserMessage:()=>{}});
  const ctx={mode:'tui',isIdle:()=>true,sessionManager:{getSessionFile:()=>sessionFile,getSessionId:()=> 'id',getSessionName:()=>undefined},ui:{notify:()=>{}}};
  try{
    await Promise.all([handlers.get('session_start')({},ctx),handlers.get('session_start')({},ctx)]);
    assert.equal(readdirSync(directory).filter(x=>x.endsWith('.json')).length,1);
    const starting=handlers.get('session_start')({},ctx);
    await handlers.get('session_shutdown')({},ctx);
    await starting;
    assert.equal(readdirSync(directory).filter(x=>x.endsWith('.json')).length,0);
    await handlers.get('session_start')({}, {...ctx,mode:'print'});
    assert.equal(readdirSync(directory).filter(x=>x.endsWith('.json')).length,0);
  }finally{
    await handlers.get('session_shutdown')({},ctx);
    if(previous===undefined)delete process.env.PI_HISTORY_BRIDGE_DIR;else process.env.PI_HISTORY_BRIDGE_DIR=previous;
    rmSync(directory,{recursive:true,force:true});
  }
});
