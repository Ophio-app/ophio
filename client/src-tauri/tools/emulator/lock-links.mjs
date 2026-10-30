// SPDX-License-Identifier: Apache-2.0
// Run only after E2E.md's isolated fixture pairing. No real device, account or inference.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {adb, companionAddress} from './adb.mjs';
import {connect, devtoolsPort, pause} from './cdp.mjs';
const lane = fileURLToPath(new URL('../../../../', import.meta.url));
const output = `${process.env.OPHIO_EVIDENCE ?? `${lane}/target/e2e-evidence`}/lock-links`;
fs.mkdirSync(output, {recursive: true});
const fixturePin = '842615'; // Disposable emulator credential, never a real phone PIN.
const connectApp = async () => {
  for (let n = 0; n < 100; n++) {
    let pid = '';
    try { pid = adb('shell', 'pidof', 'app.ophio').toString().trim(); } catch { /* Activity process is not ready yet. */ }
    if (/^\d+$/.test(pid)) {
      adb('forward', `tcp:${devtoolsPort}`, `localabstract:webview_devtools_remote_${pid}`);
      try { return await connect(); } catch { /* Cold WebView is still starting. */ }
    }
    await pause(150);
  }
  throw new Error('The isolated app WebView did not start.');
};
let c = await connectApp();
const wait = async expression => {
  for (let n = 0; n < 160; n++) { if (await c.evaluate(`Boolean(${expression})`)) return; await pause(100); }
  throw new Error('The expected emulator state did not arrive.');
};
const context = () => c.evaluate(`(()=>{const el=document.querySelector('.app');if(!el)throw new Error('Connected app unavailable');let f=el[Object.keys(el).find(k=>k.startsWith('__reactFiber'))];for(;f;f=f.return){if(f.memoizedProps?.app){window.e2eApp=f.memoizedProps.app;return;}}throw new Error('App context unavailable');})()`);
const click = label => c.evaluate(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>b.getClientRects().length&&b.textContent.trim()===${JSON.stringify(label)});if(!b)throw new Error('Expected button unavailable');b.click();})()`);
const screenshot = async name => {
  const {data} = await c.send('Page.captureScreenshot', {format:'png'});
  fs.writeFileSync(`${output}/${name}.png`, Buffer.from(data, 'base64'));
};
const credential = async () => { await pause(1200); adb('shell','input','text',fixturePin); adb('shell','input','keyevent','KEYCODE_ENTER'); await pause(900); };
const state = () => c.evaluate("window.__TAURI_INTERNALS__.invoke('app_lock_state')");
const unlock = async () => { await click('Unlock app'); await credential(); await wait("!document.querySelector('.local-lock')"); };
const view = url => adb('shell','am','start','-a','android.intent.action.VIEW','-d',url);
try {
  await context();
  assert.equal(await c.evaluate(`e2eApp.profile.addresses.includes(${JSON.stringify(companionAddress)})`), true, 'Only the isolated fixture may be used.');
  assert.equal((await state()).enabled, false, 'Start with app lock off.');
  const fixture = await c.evaluate(`(async()=>{
    const session=e2eApp.session;
    const snap=await session.query({kind:'snapshot'});
    // The full run has already registered this folder under another name.
    const folder=${JSON.stringify(path.join(lane, 'target/phone-e2e/approvals'))};
    let project=snap.projects.find(p=>p.name==='Lock and link fixture'||p.host_path===folder);
    if(!project){const r=await session.command({command:'add_project',host_path:folder,name:'Lock and link fixture'},{label:'Add fixture project'});if(r.result?.kind!=='project_added')throw new Error('Project not created');project={project_id:r.result.project_id};}
    const existing=snap.tasks.find(t=>t.project_id===project.project_id&&t.title==='Review fixture permission'&&snap.approvals.some(a=>a.task_id===t.task_id&&a.state==='pending'));
    if(existing)return {host:e2eApp.profile.hostId,task:existing.task_id};
    const r=await session.command({command:'create_task',project_id:project.project_id,kind:'coding',mode:'work',title:'Review fixture permission',prompt:'bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Start authored fixture'});
    if(r.result?.kind!=='task_created')throw new Error('Fixture task missing');
    return {host:e2eApp.profile.hostId,task:r.result.task_id};
  })()`);
  await wait(`Object.values(e2eApp.session.state.get().approvals).some(a=>a.task_id===${JSON.stringify(fixture.task)}&&a.state==='pending')`);
  fixture.approval = await c.evaluate(`Object.values(e2eApp.session.state.get().approvals).find(a=>a.task_id===${JSON.stringify(fixture.task)}&&a.state==='pending').approval_id`);
  const link = `ophio-object://open/v1/${fixture.host}/approval/${fixture.approval}`;
  const pending = () => c.evaluate(`e2eApp.session.state.get().approvals[${JSON.stringify(fixture.approval)}]?.state==='pending'`);
  // The screen lock was set while the app stayed open. A person sets it in
  // Android's settings and comes back, which is when the app looks again.
  adb('shell','input','keyevent','KEYCODE_HOME'); await pause(600);
  adb('shell','am','start','-n','app.ophio/.MainActivity'); await pause(900);
  await c.evaluate("e2eApp.ui.push({screen:'settings'})"); await wait("document.querySelector('#settings-lock')");
  await wait("[...document.querySelectorAll('label')].some(l=>l.textContent.includes('Lock the app')&&!l.querySelector('input').disabled)");
  await c.evaluate("[...document.querySelectorAll('label')].find(l=>l.textContent.includes('Lock the app')).querySelector('input').click()");
  await credential(); assert.equal((await state()).enabled, true);
  adb('shell','input','keyevent','KEYCODE_HOME'); await pause(600);
  adb('shell','input','keyevent','KEYCODE_APP_SWITCH'); await pause(1500);
  fs.writeFileSync(`${output}/recents-protected.png`, adb('exec-out','screencap','-p'));
  view(link); await wait("document.querySelector('.local-lock')");
  assert.equal(await c.evaluate("!!document.querySelector('dialog[open]')"), false);
  await screenshot('warm-locked'); await unlock();
  await wait("[...document.querySelectorAll('button')].some(b=>b.textContent==='Open item')");
  assert.equal(await pending(), true); await screenshot('warm-preview');
  await click('Open item'); await pause(700); await context();
  assert.equal(await pending(), true); await screenshot('warm-approval');
  c.close(); adb('shell','am','force-stop','app.ophio'); view(link); await pause(1600);
  const pid = adb('shell','pidof','app.ophio').toString().trim();
  if (!/^\d+$/.test(pid)) throw new Error('Packaged app process missing.');
  c=await connectApp();
  await wait("document.querySelector('.local-lock')"); assert.equal(await c.evaluate("!!document.querySelector('dialog[open]')"),false);
  await screenshot('cold-locked'); await unlock();
  await wait("[...document.querySelectorAll('button')].some(b=>b.textContent==='Open item')");
  await context(); assert.equal(await pending(),true); await screenshot('cold-preview');
  await click('Open item'); await pause(700); await context(); assert.equal(await pending(),true);
  await screenshot('cold-approval');
  await c.evaluate(`(async()=>{const a=e2eApp.session.state.get().approvals[${JSON.stringify(fixture.approval)}];await e2eApp.session.command({command:'resolve_approval',approval_id:a.approval_id,decision:{decision:'deny',reason:'Fixture cleanup'}},{label:'Deny fixture permission',expectedRevision:a.revision});})()`);
  await wait(`e2eApp.session.state.get().approvals[${JSON.stringify(fixture.approval)}]?.state!=='pending'`);
  view(link); await pause(700);
  if (await c.evaluate("!!document.querySelector('.local-lock')")) await unlock();
  await wait("document.querySelector('dialog[open]')?.textContent.toLowerCase().includes('already answered')");
  await screenshot('settled-preview');
  await c.evaluate("window.__TAURI_INTERNALS__.invoke('app_lock_configure',{enabled:false,relockMinutes:0}); true");
  await credential(); assert.equal((await state()).enabled,false);
  fs.writeFileSync(`${output}/results.json`,JSON.stringify({warm:true,cold:true,previewBeforeNavigation:true,unlockNeverApproved:true,settled:true,screenLock:'device credential',installer:await c.evaluate("e2eApp.platform.appInfo().then(i=>i.installerPackage??null)")},null,2));
  console.log('PASS: native lock, warm and cold VIEW, explicit preview, pending approval preserved, settled reference.');
} finally { c.close(); }
