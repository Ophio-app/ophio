// SPDX-License-Identifier: Apache-2.0
// Run after pairing in the app. Reads only synthetic dev-host metrics.
import { connect } from './cdp.mjs';
import fs from 'node:fs';
const c = await connect();
const metrics = JSON.parse(fs.readFileSync(new URL('../../target/dev-host-data/metrics.json', import.meta.url)));
const asset = fs.readdirSync(new URL('../../../dist/assets', import.meta.url)).find(x => /^native-.*\.js$/.test(x));
try {
  const result = await c.evaluate(`(async()=>{
    const assert=(condition,message)=>{if(!condition)throw new Error(message)};
    const wait=async(fn)=>{for(let i=0;i<250;i++){const value=fn();if(value)return value;await new Promise(r=>setTimeout(r,40));}throw new Error('Timed out waiting for transport');};
    window.testPlatform ??= (await import('/assets/${asset}')).createNativePlatform();
    window.fixtureProfile ??= (await testPlatform.profiles())[0];
    window.control?.disconnect();await window.__TAURI_INTERNALS__.invoke('link_close',{linkId:1});
    window.controlEvents=[];window.control=testPlatform.control(fixtureProfile.profileId);control.listen(e=>controlEvents.push(e));control.connect();
    await wait(()=>controlEvents.some(e=>e.type==='authenticated'));
    const command=async(body)=>{const command_id=crypto.randomUUID();assert(control.send({type:'command',command_id,issued_at:Date.now(),expected_revision:null,expires_at:null,body}),'Control refused command');const receipt=await wait(()=>controlEvents.find(e=>e.frame?.command_id===command_id)?.frame);if(receipt.error)throw new Error(receipt.error.message);return receipt.result;};
    const query_id=crypto.randomUUID();control.send({type:'query',query_id,query:{kind:'snapshot'}});
    const snapshot=await wait(()=>controlEvents.find(e=>e.frame?.query_id===query_id)?.frame.result.Ok);
    const side=async(kind)=>{const link=testPlatform[kind](fixtureProfile.profileId);const events=[];link.listen(e=>events.push(e));await wait(()=>events.some(e=>e.state?.state==='online'));return {link,events};};
    const bytes=Uint8Array.from({length:3*1024*1024},(_,i)=>i%251);
    const hash=async(b)=>Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',b))).map(x=>x.toString(16).padStart(2,'0')).join('');
    const sha256=await hash(bytes);
    const upload=await command({command:'create_upload',name:'emulator-roundtrip.bin',size:bytes.length,sha256,destination:{kind:'artifact'}});
    const up=await side('transfer');up.link.send({type:'upload',transfer_id:upload.transfer.transfer_id});
    await wait(()=>up.events.some(e=>e.frame?.type==='resume_at'));
    for(let i=0;i<bytes.length;i+=256*1024)assert(await up.link.chunk(bytes.subarray(i,i+256*1024)),'Paced write failed');
    up.link.send({type:'finish',transfer_id:upload.transfer.transfer_id});
    const completed=await wait(()=>up.events.find(e=>e.frame?.type==='completed')?.frame);assert(completed.sha256===sha256,'Upload hash mismatch');up.link.close();
    await new Promise(r=>setTimeout(r,150));
    const download=await command({command:'create_download',source:{kind:'artifact',artifact_id:completed.artifact_id}});
    const down=await side('transfer');down.link.send({type:'download',transfer_id:download.transfer.transfer_id,offset:0});
    await wait(()=>down.events.some(e=>e.frame?.type==='completed'));
    const chunks=down.events.filter(e=>e.type==='chunk').map(e=>e.data);const received=new Uint8Array(chunks.reduce((s,b)=>s+b.length,0));let offset=0;for(const b of chunks){received.set(b,offset);offset+=b.length;}assert(await hash(received)===sha256,'Download hash mismatch');down.link.close();
    const saved=await testPlatform.saveToPhone(new Blob([received]),'emulator-roundtrip.bin');
    const term=await side('terminal');term.link.send({type:'attach',terminal_id:${JSON.stringify(metrics.terminalId)},from_offset:null});await wait(()=>term.events.some(e=>e.type==='output'&&e.data.length));const terminalBytes=term.events.filter(e=>e.type==='output').reduce((s,e)=>s+e.data.length,0);term.link.close();
    const media=await side('media');media.link.send({type:'open',target_id:${JSON.stringify(metrics.targetId)},params:{max_width:1280,max_height:720,quality:60,max_fps:15,window:2}});await wait(()=>media.events.some(e=>e.type==='binary'));const mediaBytes=media.events.find(e=>e.type==='binary').data.length;media.link.close();
    return {snapshot:snapshot.kind,uploadBytes:bytes.length,downloadBytes:received.length,hashVerified:true,saved:saved.savedAs,terminalBytes,mediaBytes,keyProtection:fixtureProfile.keyProtection};
  })()`);
  console.log(JSON.stringify(result, null, 2));
} finally { c.close(); }
