// SPDX-License-Identifier: Apache-2.0
// Real binary + packaged WebView checks, with the repository's authored harnesses only.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { connect, devtoolsPort, pause } from "./cdp.mjs";
import { adb, companionAddress, tap } from "./adb.mjs";
const lane = fileURLToPath(new URL("../../../../", import.meta.url));
const evidence =
  process.env.OPHIO_EVIDENCE ?? path.join(lane, "target/e2e-evidence");
const fixture =
  process.env.OPHIO_FIXTURE ?? path.join(lane, "target/phone-e2e");
// Android labels the app with its display name, so the share sheet lists it
// under that rather than the command's name.
const appLabel = JSON.parse(
  fs.readFileSync(path.join(lane, "client/src-tauri/tauri.conf.json"), "utf8"),
).productName;
let c = await connect();
const evaluate = (expression) => c.evaluate(expression);
const waitFor = async (expression, seconds = 20) => {
  for (let n = 0; n < seconds * 12.5; n++) {
    if (await evaluate(expression)) return;
    await pause(80);
  }
  throw new Error(`UI condition timed out: ${expression}`);
};
const press = async (selector) => {
  await evaluate(
    `(()=>{const b=[...document.querySelectorAll(${JSON.stringify(selector)})].find(b=>b.getClientRects().length&&!b.closest('.is-hidden'));if(!b||b.disabled)throw new Error('Control unavailable');b.scrollIntoView({block:'nearest'});b.click();})()`,
  );
  await pause(350);
};
const fill = async (selector, text) => {
  await evaluate(
    `(()=>{const e=[...document.querySelectorAll(${JSON.stringify(selector)})].find(e=>e.getClientRects().length);if(!e)throw new Error('Field unavailable');Object.getOwnPropertyDescriptor(e.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value').set.call(e,${JSON.stringify(text)});e.dispatchEvent(new Event('input',{bubbles:true}));})()`,
  );
  await pause(150);
};
const record = (name, result) => {
  fs.writeFileSync(
    path.join(evidence, `${name}.json`),
    JSON.stringify(result, null, 2),
  );
  console.log(result);
};
const click = async (label) => {
  await evaluate(
    `(()=>{const b=[...document.querySelectorAll('button')].find(b=>!b.closest('.is-hidden') && b.getClientRects().length && b.textContent.trim()===${JSON.stringify(label)});if(!b)throw new Error('Button unavailable');b.click();})()`,
  );
  await pause(350);
};
const input = async (label, text) => {
  await evaluate(
    `(()=>{const label=[...document.querySelectorAll('label')].find(l=>l.textContent.includes(${JSON.stringify(label)}));const el=label?.querySelector('input,textarea')??document.getElementById(label?.htmlFor);if(!el)throw new Error('Input unavailable');Object.getOwnPropertyDescriptor(el.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value').set.call(el,${JSON.stringify(text)});el.dispatchEvent(new Event('input',{bubbles:true}));})()`,
  );
};
const shot = (name) =>
  fs.writeFileSync(
    path.join(evidence, `${name}.png`),
    adb("exec-out", "screencap", "-p"),
  );
const bindApp = () =>
  evaluate(
    `(()=>{const el=document.querySelector('.app');let f=el[Object.keys(el).find(k=>k.startsWith('__reactFiber'))];for(;f;f=f.return){if(f.memoizedProps?.app){window.e2eApp=f.memoizedProps.app;return;}}throw new Error('Connected app unavailable');})()`,
  );
// The app's notifications as Android records them, read from its own dump.
const shade = () =>
  adb("shell", "dumpsys", "notification", "--noredact")
    .toString()
    .split("NotificationRecord(")
    .slice(1)
    .filter((r) => r.includes("pkg=app.ophio"))
    .map((r) => ({
      tag: r.match(/ tag=(\S+)/)?.[1].replace(/^null$/, "") || null,
      channel: r.match(/channel=([\w-]+)/)?.[1] ?? null,
      title: r.match(/android\.title=\w+ \((.*)\)/)?.[1] ?? null,
      text: r.match(/android\.text=\w+ \((.*)\)/)?.[1] ?? null,
      // When Android first showed it.
      created: Number(r.match(/mCreationTimeMs=(\d+)/)?.[1] ?? 0),
    }));
// Waits until a matching notification is shown, or with `present` false,
// until none is.
const waitShade = async (match, seconds = 60, present = true) => {
  for (let n = 0; n < seconds * 2; n++) {
    const found = shade().find(match);
    if (present ? found : !found) return found;
    await pause(500);
  }
  throw new Error(present ? "Notification not shown" : "Notification not removed");
};
// Waits until the alert service runs, or with `running` false, until it
// doesn't.
const waitService = async (running = true, seconds = 30) => {
  for (let n = 0; n < seconds * 2; n++) {
    const now = adb("shell", "dumpsys", "activity", "services", "app.ophio")
      .toString()
      .includes("app.ophio/.device.AlertService");
    if (now === running) return;
    await pause(500);
  }
  throw new Error(running ? "The alert service isn't running" : "The alert service didn't stop");
};
// The app's notifications other than alerts; there should be none.
const notices = () =>
  shade().filter((n) => n.channel !== "needs-you" && n.channel !== "finished");
// Android keeps the closed app's watcher running only with Unrestricted
// battery use. A step that changes it records how it was, and the run puts
// it back however the step ends.
const exempt = () =>
  adb("shell", "dumpsys", "deviceidle", "whitelist")
    .toString()
    .includes(",app.ophio,");
const unrestricted = (on) =>
  adb("shell", "dumpsys", "deviceidle", "whitelist", on ? "+app.ophio" : "-app.ophio");
let wasExempt = null;
// Swipes the app's card away in Recents, as a person closes the app. The app
// must close cleanly: after a crash Android holds off restarting the alert
// service, by up to half an hour.
const swipeAway = async () => {
  c.close();
  const pid = adb("shell", "pidof", "app.ophio").toString().trim();
  adb("logcat", "-b", "crash", "-c");
  adb("shell", "input", "keyevent", "KEYCODE_APP_SWITCH");
  await pause(1500);
  adb("shell", "input", "swipe", "540", "1200", "540", "120", "250");
  await pause(1200);
  adb("shell", "input", "keyevent", "KEYCODE_HOME");
  await pause(500);
  if (/: app\.ophio\//.test(adb("shell", "am", "stack", "list").toString()))
    throw new Error("The app is still in Recents");
  // Android restarts the alert service in a new process within seconds.
  for (let n = 0; ; n++) {
    // pidof fails while no process is running.
    const now = adb("shell", "pidof app.ophio || true").toString().trim();
    if (now !== "" && now !== pid) break;
    if (n === 40) throw new Error("The alert service was not restarted in a new process");
    await pause(500);
  }
  if (adb("logcat", "-b", "crash", "-d").toString().includes("app.ophio"))
    throw new Error("The app crashed as it closed");
};
// The page is rebuilt when the app opens again, in a new process if Android
// ended the old one; connect to whichever page it now is.
const reopen = async () => {
  for (let n = 0; n < 60; n++) {
    try {
      const pid = adb("shell", "pidof", "app.ophio").toString().trim();
      adb("forward", `tcp:${devtoolsPort}`, `localabstract:webview_devtools_remote_${pid}`);
      const next = await connect();
      // A connection left open keeps the check from exiting.
      c.close();
      c = next;
      await bindApp();
      return;
    } catch {
      await pause(500);
    }
  }
  throw new Error("The app's page did not come back");
};
// Replaces a Library note's text the way a person does: select all, type.
const replaceNote = async (text) => {
  await waitFor("!!document.querySelector('.cm-content')");
  await evaluate("document.querySelector('.cm-content').focus()");
  for (const type of ["keyDown", "keyUp"])
    await c.send("Input.dispatchKeyEvent", {
      type,
      key: "a",
      code: "KeyA",
      windowsVirtualKeyCode: 65,
      modifiers: 2,
    });
  await c.send("Input.insertText", { text });
  await pause(250);
};
const openNote = async (name) => {
  await evaluate(
    "(()=>{const r=e2eApp.session.state.get().libraryRoots.find(r=>r.label==='Notes');e2eApp.ui.reset('library',[{screen:'folder',rootId:r.root_id,path:''}]);})()",
  );
  const row = `[...document.querySelectorAll('.tab:not(.is-hidden) button')].find(b=>b.textContent.includes(${JSON.stringify(name)}))`;
  await waitFor(`!!${row}`);
  await evaluate(`${row}.click()`);
  await pause(500);
};
// Only this checkout's isolated Ophio may be paused or inspected.
const companionPid = () => {
  const pid = Number(
    fs.readFileSync(path.join(fixture, "companion.pid"), "utf8"),
  );
  if (
    !fs
      .readFileSync(`/proc/${pid}/cmdline`, "utf8")
      .startsWith(path.join(lane, "target/debug/ophio") + "\0")
  )
    throw new Error("Not the isolated companion");
  return pid;
};
// Airplane mode drops the phone's connections as leaving coverage does.
const network = (on) =>
  adb(
    "shell",
    "cmd",
    "connectivity",
    "airplane-mode",
    on ? "disable" : "enable",
  );
// Leaving airplane mode brings cellular up first; when Wi-Fi validates,
// Android keeps cellular for a 30 s linger and then drops connections made
// over it. Waiting for Wi-Fi to be the settled default keeps that hand-over
// out of the steps that follow.
const networkSettled = async () => {
  for (let n = 0, held = 0; n < 120; n++) {
    const state = adb("shell", "dumpsys", "connectivity").toString();
    const id = /Active default network: (\d+)/.exec(state)?.[1];
    const networks = state
      .split("\n")
      .filter((line) => line.startsWith("  NetworkAgentInfo{"));
    const settled =
      networks.some(
        (line) =>
          line.startsWith(`  NetworkAgentInfo{network{${id}}`) &&
          line.includes("ni{WIFI CONNECTED") &&
          line.includes("IS_VALIDATED"),
      ) &&
      !networks.some((line) => /\b(lingering|inactive)\b/.test(line)) &&
      !/^\s+-?\d+, expires \d+ms$/m.test(state);
    held = settled ? held + 1 : 0;
    if (held === 2) return;
    await pause(1000);
  }
  throw new Error("The phone's network did not settle on Wi-Fi");
};
// A person picks a file from the phone's Downloads with Android's own picker.
const chooseFile = async (name) => {
  await evaluate("e2eApp.ui.reset('library',[{screen:'transfers'}])");
  const input = `document.querySelector('input[aria-label="Choose a file"]')`;
  await waitFor(`!!${input}`);
  await evaluate(`${input}.scrollIntoView({block:'center'})`);
  tap("Choose a file");
  await pause(1500);
  // The picker opens where it likes; a tap during the drawer's slide misses.
  for (let attempt = 0; ; attempt++) {
    tap("Show roots");
    await pause(1500);
    tap("Downloads");
    await pause(1500);
    try {
      tap(name);
      break;
    } catch (e) {
      if (attempt) throw e;
    }
  }
  await waitFor(
    `e2eApp.session.online&&document.querySelector('.transfer-footer')?.innerText.includes(${JSON.stringify(name)})&&!document.querySelector('.transfer-pick')?.innerText.includes('Keeping')`,
    60,
  );
};
const transferRow = (name) =>
  `[...document.querySelectorAll('.tab:not(.is-hidden) .transfer-row')].find(r=>r.querySelector('h3')?.textContent===${JSON.stringify(name)})`;
const rowSays = (name, text) =>
  `!!${transferRow(name)}?.innerText.includes(${JSON.stringify(text)})`;
const progressOf = (name) =>
  `(document.querySelector('progress[aria-label=${JSON.stringify(`Bytes transferred for ${name}`)}]')?.value??0)`;
// The computer's own transfer records, as it reports them to this device.
const records = (name) =>
  evaluate(
    `Object.values(e2eApp.session.state.get().transfers).filter(t=>t.name===${JSON.stringify(name)}).map(t=>({id:t.transfer_id,state:t.state.state,transferred:t.transferred,size:t.size}))`,
  );
const sha256 = (file) =>
  createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const onPhone = (name) =>
  adb("shell", "ls", "/sdcard/Download").toString().split(/\s+/).includes(name);
try {
  if (process.argv[2] !== "pair") await bindApp();
  if (process.argv[2] === "pair") {
    // `ophio pair` starts beside this step, so give it time to write its offer.
    let link = null;
    for (let i = 0; i < 60 && !link; i++) {
      if (i) await new Promise((r) => setTimeout(r, 500));
      const offerFile = path.join(fixture, "pair.txt");
      link = fs.existsSync(offerFile)
        ? fs.readFileSync(offerFile, "utf8").match(/ophio:\/\/pair\?d=([\w-]+)/)
        : null;
    }
    if (!link) throw new Error("No local pairing offer");
    const offer = JSON.parse(Buffer.from(link[1], "base64url"));
    offer.addresses = [companionAddress]; // Emulator networking only; pin and one-use proof stay unchanged.
    // A freshly installed app may still be starting its WebView.
    await waitFor(
      "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Pair a computer')",
      60,
    );
    await click("Pair a computer");
    await click("Paste the link instead");
    await input(
      "Pairing link",
      `ophio://pair?d=${Buffer.from(JSON.stringify(offer)).toString("base64url")}`,
    );
    await click("Continue");
    await input("Name this", "Build phone");
    await click("Pair");
    // The computer holds the request until its owner confirms the code the
    // phone shows. It goes to fixture storage for `ophio pair confirm`.
    await waitFor("Boolean(document.querySelector('.pair-code'))");
    const code = (
      await evaluate("document.querySelector('.pair-code').textContent")
    ).replace(/\s/g, "");
    fs.writeFileSync(path.join(fixture, "pair-code.txt"), code, {
      mode: 0o600,
    });
    shot("phone-waiting");
    console.log(`The phone shows ${code}; confirm it with ophio pair confirm.`);
    await waitFor(
      "[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Continue')",
      300,
    );
    await click("Continue");
    // The first pairing asks once about alerts while the app is closed. Not
    // now keeps Android's notification permission fresh for `permission`;
    // `background` turns them on from Settings.
    await waitFor(
      "document.body.innerText.includes('Get alerts when an agent asks something or finishes?')",
      30,
    );
    shot("phone-alerts-setup");
    await evaluate(
      "[...document.querySelectorAll('label')].find(l=>l.textContent.trim()==='Not now').querySelector('input').click()",
    );
    await click("Continue");
    await waitFor(
      "!document.body.innerText.includes('Get alerts when an agent asks something or finishes?')",
    );
    await pause(1500);
    shot("phone-paired");
    console.log(
      "Welcome paste pairing completed; no offer or key recorded in evidence.",
    );
  } else if (process.argv[2] === "work") {
    const hasProject = await evaluate(
      "Object.keys(e2eApp.session.state.get().projects).length>0",
    );
    if (!hasProject) {
      await evaluate("e2eApp.ui.reset('talk')");
      await pause(500);
      await input("Folder on the computer", path.join(fixture, "project"));
      await input("Name (optional)", "Phone fixture");
      await click("Add project");
      await pause(800);
    }
    const result = await evaluate(`(async()=>{
      const app=e2eApp, session=app.session;
      const wait=async(fn)=>{for(let n=0;n<300;n++){const v=fn();if(v)return v;await new Promise(r=>setTimeout(r,40));}throw new Error('Timed out waiting for companion state');};
      const snap=await session.query({kind:'snapshot'}), project=snap.projects.find(p=>p.name==='Phone fixture');
      if(!project||!session.online||!session.state.get().caughtUp)throw new Error('Snapshot/authentication missing');
      window.e2eProject=project.project_id;app.ui.selectProject(project.project_id);
      window.e2eStart=async(harness,prompt,title)=>{
        const r=await session.command({command:'create_task',project_id:window.e2eProject,kind:'coding',mode:'work',title,prompt,bundle:null,harness,model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Start fixture task'});
        if(r.result?.kind!=='task_created')throw new Error('Task not created');
        app.ui.selectTask(window.e2eProject,r.result.task_id);app.ui.reset('talk');return r.result.task_id;
      };
      let streamed=false;const off=session.state.subscribe(()=>{if(Object.keys(session.state.get().liveText).length)streamed=true;});
      const existing=Object.values(session.state.get().tasks).find(t=>t.title==='Streaming fixture');
      const task=existing?.task_id??await e2eStart('codex','fixture work','Streaming fixture');
      if(!existing)await wait(()=>Object.values(session.state.get().approvals).some(a=>a.task_id===task&&a.state==='pending'));
      if(existing)streamed=(await session.query({kind:'task_timeline',task_id:task,before_seq:null,limit:80})).items.some(i=>i.item.kind==='agent_message');
      app.ui.selectTask(project.project_id,task);app.ui.reset('talk');
      // The named Stop also says which display the task holds, if any.
      const stop=()=>[...document.querySelectorAll('button')].find(b=>b.getClientRects().length&&b.getAttribute('aria-label')?.startsWith('Stop · Streaming fixture'));
      (await wait(stop)).click();
      await wait(()=>session.state.get().tasks[task]?.state==='interrupted');
      await wait(()=>session.state.get().runs[session.state.get().tasks[task].current_run]?.state==='interrupted');
      await wait(()=>!Object.values(session.state.get().approvals).some(a=>a.task_id===task&&a.state==='pending'));off();
      const approvalProject=(await session.command({command:'add_project',host_path:${JSON.stringify(path.join(fixture, "approvals"))},name:'Phone approvals'},{label:'Add approval fixture'})).result;
      window.e2eProject=approvalProject.project_id;
      const decisions=[];
      for(const decision of ['allow','deny']){
        const id=await e2eStart('claude_code','bridge-permission','Permission '+decision);
        const a=await wait(()=>Object.values(session.state.get().approvals).find(a=>a.task_id===id&&a.state==='pending'));
        await session.command({command:'resolve_approval',approval_id:a.approval_id,decision:decision==='allow'?{decision:'allow'}:{decision:'deny',reason:'Fixture denial'}},{label:'Fixture decision',expectedRevision:a.revision});
        await wait(()=>session.state.get().tasks[id]?.state==='completed');
        const timeline=await session.query({kind:'task_timeline',task_id:id,before_seq:null,limit:80});
        if(!JSON.stringify(timeline).includes('Permission '+decision))throw new Error('Native decision round trip missing');
        decisions.push({decision,taskId:id,approvalState:session.state.get().approvals[a.approval_id]?.state});
      }
      return {authenticated:true,snapshotHead:snap.journal_head,projectSelected:app.ui.get().projectId===window.e2eProject,streamed,stopState:session.state.get().tasks[task]?.state,decisions};
    })()`);
    fs.writeFileSync(
      path.join(evidence, "work.json"),
      JSON.stringify(result, null, 2),
    );
    console.log(result);
    await pause(500);
    shot("phone-tasks");
    for (const decision of result.decisions) {
      await evaluate(
        `e2eApp.ui.selectTask(e2eApp.session.state.get().tasks[${JSON.stringify(decision.taskId)}].project_id,${JSON.stringify(decision.taskId)});e2eApp.ui.reset('talk')`,
      );
      await pause(500);
      shot(`phone-approval-${decision.decision}`);
    }
  } else if (process.argv[2] === "data") {
    const result = await evaluate(String.raw`(async()=>{
      const {session,platform,profile,ui}=e2eApp;
      const wait=async(fn)=>{for(let n=0;n<350;n++){const v=fn();if(v)return v;await new Promise(r=>setTimeout(r,40));}throw new Error('Side connection timed out');};
      const command=async body=>{const r=await session.command(body,{label:'Phone data check'});if(r.result)return r.result;for(let i=0;i<200;i++){const s=await session.query({kind:'command_status',command_id:r.command_id});if(s.error)throw new Error(s.error.message);if(s.stage==='observed')return s.result;await new Promise(r=>setTimeout(r,40));}throw new Error('Effect not observed');};
      const root=session.state.get().libraryRoots.find(r=>r.label==='Notes');if(!root)throw new Error('Notes root missing');
      const listing=await session.query({kind:'library_list',root_id:root.root_id,dir:''});
      const {file}=await session.query({kind:'library_read',root_id:root.root_id,path:'guide.md'});
      const save={command:'library_save',root_id:root.root_id,path:'guide.md',text:'# Saved from phone\n',base_revision:file.revision,newline:file.newline,bom:file.has_bom};
      const saved=await command(save);if(saved?.outcome?.outcome!=='saved')throw new Error('Library save failed');
      const conflict=await command({...save,text:'Stale phone text'});if(conflict?.outcome?.outcome!=='conflict')throw new Error('Library conflict missing');
      window.e2eRoot=root.root_id;ui.push({screen:'file',rootId:root.root_id,path:'guide.md'},'library');
      const side=async kind=>{const link=platform[kind](profile.profileId),events=[];link.listen(e=>events.push(e));await wait(()=>events.some(e=>e.state?.state==='online'));return {link,events};};
      const terminal=await command({command:'open_terminal',project_id:window.e2eProject??Object.keys(session.state.get().projects)[0],harness:null,cols:80,rows:24});
      const term=await side('terminal');term.link.send({type:'attach',terminal_id:terminal.terminal.terminal_id,from_offset:null});await wait(()=>term.events.some(e=>e.frame?.type==='attached'));
      term.link.input(new TextEncoder().encode("printf 'PHONE_%s\\n' PTY_OK"+String.fromCharCode(10)));
      await wait(()=>new TextDecoder().decode(new Uint8Array(term.events.filter(e=>e.type==='output').flatMap(e=>[...e.data]))).includes('PHONE_PTY_OK'));
      term.link.close();await command({command:'close_terminal',terminal_id:terminal.terminal.terminal_id});
      const bytes=Uint8Array.from({length:3*1024*1024},(_,i)=>i%251);
      const hash=async b=>Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',b))).map(x=>x.toString(16).padStart(2,'0')).join('');const sha=await hash(bytes);
      const upload=await command({command:'create_upload',name:'phone-roundtrip.bin',size:bytes.length,sha256:sha,destination:{kind:'artifact'}});
      const up=await side('transfer');up.link.send({type:'upload',transfer_id:upload.transfer.transfer_id});await wait(()=>up.events.some(e=>e.frame?.type==='resume_at'));
      for(let i=0;i<bytes.length;i+=256*1024)if(!await up.link.chunk(bytes.subarray(i,i+256*1024)))throw new Error('Upload write failed');
      up.link.send({type:'finish',transfer_id:upload.transfer.transfer_id});const completed=await wait(()=>up.events.find(e=>e.frame?.type==='completed')?.frame);up.link.close();
      const download=await command({command:'create_download',source:{kind:'artifact',artifact_id:completed.artifact_id}});
      const down=await side('transfer');down.link.send({type:'download',transfer_id:download.transfer.transfer_id,offset:0});await wait(()=>down.events.some(e=>e.frame?.type==='completed'));
      const chunks=down.events.filter(e=>e.type==='chunk').map(e=>e.data);const received=new Uint8Array(chunks.reduce((s,b)=>s+b.length,0));let at=0;for(const b of chunks){received.set(b,at);at+=b.length;}down.link.close();
      if(completed.sha256!==sha||await hash(received)!==sha||received.length!==bytes.length)throw new Error('Checksum mismatch');
      const savedPhone=await platform.saveToPhone(new Blob([received]),'phone-roundtrip.bin');
      return {libraryListed:!!listing,librarySaved:saved.outcome.outcome,staleSave:conflict.outcome.outcome,terminal:true,uploadBytes:bytes.length,downloadBytes:received.length,sha256:sha,savedAs:savedPhone.savedAs,keyProtection:profile.keyProtection};
    })()`);
    fs.writeFileSync(
      path.join(evidence, "data.json"),
      JSON.stringify(result, null, 2),
    );
    console.log(result);
    await pause(600);
    shot("phone-library");
    await evaluate("e2eApp.ui.reset('library',[{screen:'transfers'}])");
    await pause(600);
    shot("phone-transfers");
  } else if (process.argv[2] === "library-ui") {
    fs.writeFileSync(path.join(fixture, "notes", "ui-note.md"), "# UI note\n");
    await openNote("ui-note.md");
    await evaluate(
      "[...document.querySelectorAll('button')].find(b=>!b.closest('.is-hidden')&&['Edit','Continue editing draft'].includes(b.textContent)).click()",
    );
    await pause(1000);
    await replaceNote("# UI saved from phone\n");
    await click("Review changes");
    await click("Save on computer");
    await pause(900);
    const saved =
      fs.readFileSync(path.join(fixture, "notes", "ui-note.md"), "utf8") ===
      "# UI saved from phone\n";
    if (!saved) throw new Error("UI save missing on computer");
    shot("phone-library-ui-saved");
    await click("Edit");
    await pause(400);
    await replaceNote("# Phone draft kept\n");
    fs.writeFileSync(
      path.join(fixture, "notes", "ui-note.md"),
      "# Concurrent computer edit\n",
    );
    await click("Review changes");
    await click("Save on computer");
    await pause(900);
    const conflict = await evaluate(
      "e2eApp.ui.get().stacks.library.at(-1)?.screen==='conflict'",
    );
    if (!conflict) throw new Error("UI conflict route missing");
    shot("phone-library-ui-conflict");
    fs.writeFileSync(
      path.join(evidence, "library-ui.json"),
      JSON.stringify({
        saved,
        conflict,
        computerPreserved:
          fs.readFileSync(path.join(fixture, "notes", "ui-note.md"), "utf8") ===
          "# Concurrent computer edit\n",
      }),
    );
    console.log({ saved, conflict });
  } else if (process.argv[2] === "stream") {
    fs.mkdirSync(path.join(fixture, "stream"), { recursive: true });
    const result = await evaluate(`(async()=>{
      const s=e2eApp.session,p=(await s.command({command:'add_project',host_path:${JSON.stringify(path.join(fixture, "stream"))},name:'Phone stream'},{label:'Stream project'})).result;
      let deltaSeen=false;const off=s.state.subscribe(()=>{if(Object.values(s.state.get().liveText).some(t=>t.text?.includes('hello')))deltaSeen=true;});
      const task=(await s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:'Live stream fixture',prompt:'later',bundle:null,harness:'codex',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Live stream'})).result.task_id;
      for(let i=0;i<250&&s.state.get().tasks[task]?.state!=='completed';i++)await new Promise(r=>setTimeout(r,30));off();
      if(!deltaSeen||s.state.get().tasks[task]?.state!=='completed')throw new Error('Live stream or completion missing');
      const timeline=await s.query({kind:'task_timeline',task_id:task,before_seq:null,limit:80});
      if(!timeline.items.some(i=>i.item.kind==='agent_message'&&i.item.text.includes('hello')))throw new Error('Durable agent reply missing');
      e2eApp.ui.selectTask(p.project_id,task);e2eApp.ui.reset('talk');
      return {deltaSeen,completed:s.state.get().tasks[task]?.state==='completed',messageSeen:JSON.stringify(timeline).includes('hello')};
    })()`);
    fs.writeFileSync(
      path.join(evidence, "stream.json"),
      JSON.stringify(result),
    );
    console.log(result);
    await pause(600);
    shot("phone-stream");
  } else if (process.argv[2] === "share") {
    const before = await evaluate(
      "({tasks:Object.keys(e2eApp.session.state.get().tasks).length,route:e2eApp.ui.get().tab})",
    );
    adb(
      "shell",
      "am",
      "start",
      "-n",
      "app.ophio/.MainActivity",
      "-a",
      "android.intent.action.SEND",
      "-t",
      "text/plain",
      "--es",
      "android.intent.extra.TEXT",
      "Phone_share_fixture",
    );
    await pause(1000);
    const intake = await evaluate(
      `(()=>{const changed=Object.keys(e2eApp.session.state.get().tasks).length!==${before.tasks};if(changed)throw new Error('Share started a task');e2eApp.ui.push({screen:'inbox'},'work');return {tasksUnchanged:!changed};})()`,
    );
    await pause(800);
    if (
      !(await evaluate(
        "document.body.innerText.includes('Phone_share_fixture')",
      ))
    )
      throw new Error("Android share missing from inbox");
    shot("phone-share-inbox");
    await click("Use in a message");
    await evaluate(
      "[...document.querySelectorAll('.sheet button')].find(b=>b.textContent.includes('Phone approvals')).click()",
    );
    await pause(800);
    const draft = await evaluate(
      "(()=>{const p=Object.values(e2eApp.session.state.get().projects).find(p=>p.name==='Phone approvals');return e2eApp.ui.draft(p.project_id,null)?.text.includes('Phone_share_fixture');})()",
    );
    if (!draft) throw new Error("Share missing from Talk draft");
    shot("phone-share-draft");
    fs.writeFileSync(
      path.join(evidence, "share.json"),
      JSON.stringify({ ...intake, draft: true }),
    );
    console.log({ ...intake, draft: true });
  } else if (process.argv[2] === "notification") {
    const id = await evaluate(`(async()=>{
      const s=e2eApp.session,p=Object.values(s.state.get().projects).find(p=>p.name==='Phone approvals');
      window.e2eNotificationPromise=new Promise(resolve=>{const off=s.state.subscribe(()=>{const a=Object.values(s.state.get().attention).find(a=>a.task_id===window.e2eNotifyTask&&a.kind==='approval'&&a.resolved_at===null);if(a){off();resolve(a.attention_id);}});});
      window.e2eNotifyStart=()=>s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:'Private title omitted from notification',prompt:'bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Notification fixture'}).then(r=>{window.e2eNotifyTask=r.result.task_id;});
      return p.project_id;
    })()`);
    adb("shell", "input", "keyevent", "KEYCODE_HOME");
    await pause(700);
    if ((await evaluate("document.visibilityState")) !== "hidden")
      throw new Error("App is not hidden");
    await evaluate("e2eNotifyStart()");
    const attentionId = await evaluate("e2eNotificationPromise");
    await pause(1800);
    adb("shell", "cmd", "statusbar", "expand-notifications");
    await pause(500);
    shot("phone-task-notification");
    tap("Approval waiting");
    // Resuming a backgrounded WebView on the emulator's software GPU can take
    // longer than a fixed pause, so wait for the app to settle on a screen.
    const tapped = Date.now();
    let opened;
    for (let n = 0; n < 125; n++) {
      opened = await evaluate(
        `({route:e2eApp.ui.get().stacks.work.at(-1),pending:Object.values(e2eApp.session.state.get().approvals).some(a=>a.task_id===window.e2eNotifyTask&&a.state==='pending')})`,
      );
      if (opened.route?.screen === "attention") break;
      await pause(80);
    }
    const openedAfterMs = Date.now() - tapped;
    if (
      opened.route?.screen !== "attention" ||
      opened.route?.attentionId !== attentionId ||
      !opened.pending
    )
      throw new Error("Notification open changed or lost the pending item");
    shot("phone-notification-item");
    // Answer it, or it holds the project until it expires ten minutes later
    // and a later step can't start a task there.
    await evaluate(`(async()=>{
      const s=e2eApp.session,a=Object.values(s.state.get().approvals).find(a=>a.task_id===window.e2eNotifyTask&&a.state==='pending');
      await s.command({command:'resolve_approval',approval_id:a.approval_id,decision:{decision:'deny',reason:'Fixture denial'}},{label:'Fixture decision',expectedRevision:a.revision});
    })()`);
    await waitFor(
      "Object.values(e2eApp.session.state.get().projects).find(p=>p.name==='Phone approvals').active_writer===null",
    );
    console.log({ projectId: id, exactItem: true, stillPending: true, openedAfterMs });
    fs.writeFileSync(
      path.join(evidence, "notification.json"),
      JSON.stringify({ exactItem: true, stillPending: true, openedAfterMs }),
    );
  } else if (process.argv[2] === "permission") {
    await evaluate(
      "(()=>{const t=Object.values(e2eApp.session.state.get().tasks).find(t=>t.title==='Streaming fixture');e2eApp.ui.selectTask(t.project_id,t.task_id);e2eApp.ui.reset('talk');})()",
    );
    await waitFor("!!document.querySelector('.alerts-offer')");
    await evaluate(
      "document.querySelector('.alerts-offer').scrollIntoView({block:'center'})",
    );
    shot("phone-alerts-offer");
    await click("Allow alerts");
    await pause(500);
    shot("phone-notification-permission");
    // On a fresh install Android presents the native explanation and permission.
    tap("Continue");
    await pause(400);
    tap("Allow");
    await pause(500);
    await evaluate(
      "e2eApp.ui.push({screen:'settings',section:'settings-notifications'})",
    );
    await pause(600);
    await waitFor("document.body.innerText.includes('Alerts are on')");
    shot("phone-notifications-settings");
    record("permission", {
      inContextOffer: true,
      nativePermission: true,
      granted: true,
    });
  } else if (process.argv[2] === "background") {
    await waitFor("e2eApp.session.online");
    const { profileId, name } = await evaluate(
      "({profileId:e2eApp.profile.profileId,name:e2eApp.profile.name})",
    );
    const pid = adb("shell", "pidof", "app.ophio").toString().trim();
    await evaluate(
      "e2eApp.ui.push({screen:'settings',section:'settings-notifications'})",
    );
    await pause(700);
    const box =
      "[...document.querySelectorAll('label')].find(l=>l.textContent.includes('Alerts when the app is closed')).querySelector('input')";
    // Turning alerts on asks for Unrestricted battery use.
    wasExempt = exempt();
    unrestricted(true);
    // A run stopped part way leaves them on; start from off.
    if (await evaluate(`${box}.checked`)) {
      await evaluate(`${box}.click()`);
      await waitService(false);
    }
    // A change still being saved disables the switch.
    await waitFor(`!${box}.disabled&&!${box}.checked`);
    await evaluate(`${box}.click()`);
    await waitFor(
      "document.body.innerText.includes('Ready for alerts from')",
      30,
    );
    await waitService();
    // Android once required a notice for this; give one a chance to appear.
    await pause(10000);
    const noticesWhileOn = notices();
    if (noticesWhileOn.length) throw new Error(`A notice is shown: ${JSON.stringify(noticesWhileOn)}`);
    shot("phone-closed-alerts-on");
    const start = (title) =>
      evaluate(`(async()=>{
        const s=e2eApp.session,p=Object.values(s.state.get().projects).find(p=>p.name==='Phone approvals');
        const r=await s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:${JSON.stringify(title)},prompt:'later 12 bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Closed-app alert fixture'});
        return r.result.task_id;
      })()`);
    // The approval comes after the app is swiped away, so only the watcher
    // can raise it.
    const first = await start("Private title kept off the lock screen");
    await swipeAway();
    const raised = await waitShade((n) => n.title === "Approval waiting");
    const [tagProfile, attentionId] = raised.tag.split("/");
    if (
      tagProfile !== profileId ||
      !/^[0-9a-f-]{36}$/.test(attentionId) ||
      raised.channel !== "needs-you" ||
      raised.text !== `${name} · Phone approvals`
    )
      throw new Error(`Unexpected alert: ${JSON.stringify(raised)}`);
    const survived =
      adb("shell", "pidof", "app.ophio").toString().trim() === pid;
    adb("shell", "cmd", "statusbar", "expand-notifications");
    await pause(500);
    shot("phone-closed-alert");
    tap("Approval waiting");
    await reopen();
    await waitFor(
      `e2eApp.ui.get().stacks.work.at(-1)?.screen==='attention'&&e2eApp.ui.get().stacks.work.at(-1)?.attentionId===${JSON.stringify(attentionId)}`,
      30,
    );
    shot("phone-closed-alert-opened");
    // On screen, the app shows what follows itself; the watcher stays quiet.
    const before = new Set(shade().map((n) => n.tag));
    await evaluate(`(async()=>{
      const s=e2eApp.session,a=Object.values(s.state.get().approvals).find(a=>a.task_id===${JSON.stringify(first)}&&a.state==='pending');
      await s.command({command:'resolve_approval',approval_id:a.approval_id,decision:{decision:'deny',reason:'Fixture denial'}},{label:'Fixture decision',expectedRevision:a.revision});
    })()`);
    await waitFor(
      "Object.values(e2eApp.session.state.get().projects).find(p=>p.name==='Phone approvals').active_writer===null",
    );
    await pause(3000);
    const quietOnScreen = !shade().some((n) => !before.has(n.tag) && n.title !== null);
    if (!quietOnScreen) throw new Error("An alert was raised while the app was on screen");
    // Resolved on the computer while the app is closed: the alert goes away.
    const second = await start("Closed-app alert resolved elsewhere");
    await swipeAway();
    const other = await waitShade(
      (n) => n.title === "Approval waiting" && n.tag !== raised.tag,
    );
    execFileSync(
      path.join(lane, "client/src-tauri/tools/emulator/isolated-companion.sh"),
      [path.join(lane, "target/debug/ophio"), "stop-task", second],
      { stdio: "pipe" },
    );
    await waitShade((n) => n.tag === other.tag, 30, false);
    await pause(2000);
    const afterStop = shade();
    shot("phone-closed-alert-resolved");
    // One still waiting when alerts are turned off goes with them.
    adb("shell", "am", "start", "-W", "-n", "app.ophio/.MainActivity");
    await reopen();
    await waitFor("e2eApp.session.online", 30);
    const third = await start("Closed-app alert when turned off");
    await swipeAway();
    const left = await waitShade(
      (n) => n.title === "Approval waiting" && n.tag !== raised.tag && n.tag !== other.tag,
    );
    adb("shell", "am", "start", "-W", "-n", "app.ophio/.MainActivity");
    await reopen();
    await waitFor("e2eApp.session.online", 30);
    await evaluate(
      "e2eApp.ui.push({screen:'settings',section:'settings-notifications'})",
    );
    await pause(700);
    await evaluate(`${box}.click()`);
    await waitFor(
      "document.body.innerText.includes('Off: alerts arrive only while the app is open.')",
    );
    await waitService(false);
    await waitShade((n) => n.tag === left.tag, 15, false);
    execFileSync(
      path.join(lane, "client/src-tauri/tools/emulator/isolated-companion.sh"),
      [path.join(lane, "target/debug/ophio"), "stop-task", third],
      { stdio: "pipe" },
    );
    await evaluate(
      "document.querySelector('#settings-versions').scrollIntoView({block:'center'})",
    );
    await pause(300);
    shot("phone-app-info");
    await evaluate(
      "[...document.querySelectorAll('*')].find(e=>e.childElementCount===0&&e.textContent.includes('Keystore wrapping key'))?.scrollIntoView({block:'center'})",
    );
    await pause(300);
    shot("phone-key-protection");
    record("background", {
      raisedWhileClosed: true,
      processSurvivedSwipe: survived,
      openedExactItem: true,
      quietOnScreen,
      cancelledWhenResolvedElsewhere: true,
      afterStop: afterStop.map((n) => ({ title: n.title, channel: n.channel })),
      noticesWhileOn: noticesWhileOn.length,
      offStopsService: true,
      offClearsRaised: true,
    });
  } else if (process.argv[2] === "question") {
    // An agent's question, answered from its notification while the app is
    // closed: once with a choice's button, once in the person's own words.
    await waitFor("e2eApp.session.online");
    await evaluate(
      "e2eApp.ui.push({screen:'settings',section:'settings-notifications'})",
    );
    await pause(700);
    const box =
      "[...document.querySelectorAll('label')].find(l=>l.textContent.includes('Alerts when the app is closed')).querySelector('input')";
    wasExempt = exempt();
    unrestricted(true);
    await waitFor(`!${box}.disabled`);
    const wasOn = await evaluate(`${box}.checked`);
    if (!wasOn) {
      await evaluate(`${box}.click()`);
      await waitFor(
        "document.body.innerText.includes('Ready for alerts from')",
        30,
      );
    }
    await waitService();
    const approvals = path.join(fixture, "approvals");
    const request = path.join(approvals, "permission-request.json");
    const result = path.join(approvals, "permission-result.json");
    // The fixture asks it once the app is closed, and later steps' fixtures
    // ask for a command again.
    const ask = async (title, question, options) => {
      fs.rmSync(result, { force: true });
      fs.writeFileSync(
        request,
        JSON.stringify({
          tool_name: "AskUserQuestion",
          input: {
            questions: [
              {
                question,
                header: "Fixture",
                options: options.map((label) => ({ label })),
                multiSelect: false,
              },
            ],
          },
        }),
      );
      try {
        // The app is asked to start it, then closed.
        adb("shell", "am", "start", "-W", "-n", "app.ophio/.MainActivity");
        await reopen();
        await waitFor("e2eApp.session.online", 30);
        const task = await evaluate(`(async()=>{
          const s=e2eApp.session,p=Object.values(s.state.get().projects).find(p=>p.name==='Phone approvals');
          const r=await s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:${JSON.stringify(title)},prompt:'later 12 bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Question fixture'});
          return r.result.task_id;
        })()`);
        await swipeAway();
        const asked = await waitShade((n) => n.text === question);
        if (!asked.title?.startsWith("Question from") || asked.channel !== "needs-you")
          throw new Error(`Unexpected alert: ${JSON.stringify(asked)}`);
        return { task, asked };
      } finally {
        fs.rmSync(request, { force: true });
      }
    };
    // What the agent was told, once it has been.
    const answered = async (question) => {
      for (let n = 0; n < 60 && !fs.existsSync(result); n++) await pause(500);
      return JSON.parse(fs.readFileSync(result, "utf8")).updatedInput?.answers?.[question];
    };
    const crashed = () =>
      adb("logcat", "-b", "crash", "-d").toString().includes("app.ophio");

    // Two short choices are buttons.
    const repo = "Create a private repo?";
    const first = await ask("Question answered with a button", repo, ["Yes", "No"]);
    adb("shell", "cmd", "statusbar", "expand-notifications");
    await pause(1000);
    shot("phone-question-asked");
    tap("Yes");
    await waitShade((n) => n.tag === first.asked.tag, 30, false);
    const chose = await answered(repo);
    if (chose !== "Yes") throw new Error(`The agent was told ${JSON.stringify(chose)}`);
    adb("shell", "cmd", "statusbar", "collapse");
    await pause(500);

    // More are offered in the reply box, where the person may write their own.
    const name = "Name the repo?";
    const second = await ask("Question answered in words", name, ["clankerhub", "hub", "tools"]);
    adb("shell", "cmd", "statusbar", "expand-notifications");
    await pause(1000);
    tap("Answer");
    await pause(1000);
    adb("shell", "input", "text", "ophio-tools");
    await pause(500);
    shot("phone-question-reply");
    tap("Send");
    await waitShade((n) => n.tag === second.asked.tag, 30, false);
    const wrote = await answered(name);
    if (wrote !== "ophio-tools") throw new Error(`The agent was told ${JSON.stringify(wrote)}`);
    adb("shell", "cmd", "statusbar", "collapse");
    if (crashed()) throw new Error("The app crashed");

    // The app agrees: both answered, both tasks done.
    adb("shell", "am", "start", "-W", "-n", "app.ophio/.MainActivity");
    await reopen();
    await waitFor("e2eApp.session.online", 30);
    const tasks = JSON.stringify([first.task, second.task]);
    await waitFor(
      `${tasks}.every(t=>e2eApp.session.state.get().tasks[t]?.state==='completed')`,
      30,
    );
    const states = await evaluate(
      `Object.values(e2eApp.session.state.get().approvals).filter(a=>${tasks}.includes(a.task_id)).map(a=>a.state)`,
    );
    if (states.length !== 2 || states.some((s) => s !== "allowed"))
      throw new Error(`Approvals: ${JSON.stringify(states)}`);
    if (!wasOn) {
      await evaluate(
        "e2eApp.ui.push({screen:'settings',section:'settings-notifications'})",
      );
      await pause(700);
      await evaluate(`${box}.click()`);
      await waitService(false);
    }
    record("question", {
      raisedWhileClosed: true,
      buttonAnswer: chose,
      writtenAnswer: wrote,
      alertsCleared: true,
      approvals: states,
      crashed: false,
    });
  } else if (process.argv[2] === "closed-lifecycle") {
    // Android restarts, updates, idles or cuts off a closed app in several
    // ways. In each, an approval asked meanwhile raises an alert, one already
    // showing is still there afterwards, and answering it at the computer
    // takes it away. Without Unrestricted battery use the approval waits for
    // the app instead. A comma-separated list after the step runs some cases.
    const apk = path.join(
      lane,
      "client/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk",
    );
    const approvals =
      "Object.values(e2eApp.session.state.get().projects).find(p=>p.name==='Phone approvals')";
    const open = async () => {
      adb("shell", "wm", "dismiss-keyguard");
      adb("shell", "am", "start", "-W", "-n", "app.ophio/.MainActivity");
      await reopen();
      await waitFor("e2eApp.session.online", 60);
    };
    const start = (title, seconds) =>
      evaluate(`(async()=>{
        const s=e2eApp.session,p=${approvals};
        const r=await s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:${JSON.stringify(title)},prompt:'later ${seconds} bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Closed-app alert fixture'});
        return r.result.task_id;
      })()`);
    const stopTask = (task) =>
      execFileSync(
        path.join(lane, "client/src-tauri/tools/emulator/isolated-companion.sh"),
        [path.join(lane, "target/debug/ophio"), "stop-task", task],
        { stdio: "pipe" },
      );
    const booted = async () => {
      for (let n = 0; n < 240; n++) {
        try {
          if (adb("shell", "getprop", "sys.boot_completed").toString().trim() === "1") return;
        } catch {
          // adb loses the emulator while it restarts.
        }
        await pause(1000);
      }
      throw new Error("The emulator did not finish starting");
    };
    const deepIdle = () =>
      adb("shell", "dumpsys", "deviceidle", "get", "deep").toString().trim();
    // Turning alerts on asks for Unrestricted battery use; one case takes
    // it away.
    wasExempt = exempt();
    unrestricted(true);
    // What each case does once the app is closed; returns what it saw.
    const events = {
      reboot: async () => {
        const at = Date.now();
        adb("reboot");
        await pause(5000);
        await booted();
        return { restartSeconds: Math.round((Date.now() - at) / 1000) };
      },
      update: async () => {
        adb("install", "-r", apk);
        return {};
      },
      doze: async () => {
        adb("shell", "dumpsys", "battery", "unplug");
        adb("shell", "input", "keyevent", "KEYCODE_SLEEP");
        adb("shell", "dumpsys", "deviceidle", "force-idle");
        return { idle: deepIdle(), unrestricted: exempt() };
      },
      // Android stops the watcher once the closed app is idle, as it does
      // a minute after the app leaves the screen; the alert waits for the app.
      limited: async () => {
        unrestricted(false);
        adb("shell", "am", "make-uid-idle", "app.ophio");
        await waitService(false, 90);
        return { unrestricted: exempt() };
      },
      airplane: async (seconds) => {
        network(false);
        // The approval is asked while the phone is offline.
        await pause((seconds + 20) * 1000);
        const whileOffline = shade().some((n) => n.title === "Approval waiting");
        network(true);
        await networkSettled();
        return { raisedWhileOffline: whileOffline, backAt: Date.now() };
      },
    };
    const wake = () => {
      adb("shell", "dumpsys", "deviceidle", "unforce");
      adb("shell", "dumpsys", "battery", "reset");
      unrestricted(true);
      adb("shell", "input", "keyevent", "KEYCODE_WAKEUP");
    };
    const cases = {
      "reboot-asked-after": ["reboot", 90, false],
      "reboot-showing-before": ["reboot", 3, true],
      "update-asked-after": ["update", 45, false],
      "update-showing-before": ["update", 3, true],
      doze: ["doze", 30, false],
      "battery-limited": ["limited", 120, false],
      "airplane-asked-offline": ["airplane", 15, false],
    };
    const chosen = process.argv[3]?.split(",") ?? Object.keys(cases);
    const results = {};
    await waitFor("e2eApp.session.online", 60);
    if (!(await evaluate("e2eApp.platform.alerts.state()")).settings?.enabled)
      await evaluate("e2eApp.platform.alerts.configure({enabled:true,detail:'minimal'})");
    await waitService();
    // A case that fails leaves its task asking; it is stopped at the end.
    let running = null;
    try {
      for (const name of chosen) {
        const [event, seconds, before] = cases[name];
        await waitFor(`${approvals}.active_writer===null`, 60);
        // The app can reconnect once more after the previous case reopened it.
        await waitFor("e2eApp.session.online", 60);
        const earlier = new Set(shade().map((n) => n.tag));
        const task = await start(`Closed-app ${name}`, seconds);
        running = task;
        // The fixture agent asks this long after it starts, within a second.
        const asked = Date.now() + seconds * 1000;
        await swipeAway();
        const fresh = (n) => n.title === "Approval waiting" && !earlier.has(n.tag);
        let result = {};
        let raised;
        if (event === "limited") {
          result = await events.limited();
          // Past when it asks, and as long again for an alert to show.
          await pause(Math.max(0, asked - Date.now()) + 30000);
          result.raisedWhileClosed = shade().some(fresh);
          if (result.raisedWhileClosed) throw new Error("An alert was raised with battery use limited");
          // Opening the app starts the watcher again, still without it, and
          // the approval is there waiting.
          await open();
          await waitService();
          result.serviceWhenOpened = true;
          await waitFor(
            `Object.values(e2eApp.session.state.get().approvals).some(a=>a.task_id===${JSON.stringify(task)}&&a.state==='pending')`,
            30,
          );
          result.waitingWhenOpened = true;
          unrestricted(true);
          stopTask(task);
          running = null;
          results[name] = result;
          console.log(name, result);
          continue;
        }
        if (before) {
          raised = await waitShade(fresh, seconds + 60);
          result = await events[event](seconds);
          // A restart or an update clears Android's shade; the watcher shows
          // it again. Until Android clears it after an update, the one from
          // before is still there.
          const again = await waitShade(
            (n) => n.tag === raised.tag && n.created > raised.created,
            90,
          ).catch(() => null);
          result.showingAfter = !!again;
          if (!again) throw new Error("The alert showing before was not raised again");
        } else {
          result = await events[event](seconds);
          raised = await waitShade(fresh, seconds + 120);
        }
        const shownAt = Date.now();
        result.processAtAlert = adb("shell", "pidof app.ophio || true").toString().trim() !== "";
        if (event === "doze") {
          result.idleAtAlert = deepIdle();
          wake();
        }
        stopTask(task);
        running = null;
        const answered = Date.now();
        if (result.showingAfter !== false) {
          await waitShade((n) => n.tag === raised.tag, 60, false);
          result.removedSecondsAfterAnswer = Math.round((Date.now() - answered) / 100) / 10;
        }
        await open();
        if (!before) result.shownSecondsAfterAsked = Math.round((shownAt - asked) / 1000);
        if (result.backAt) {
          result.shownSecondsAfterNetwork = Math.round((shownAt - result.backAt) / 100) / 10;
          delete result.backAt;
          // The watcher tries again as soon as Android reports the network,
          // not when its wait after failing offline runs out.
          if (result.shownSecondsAfterNetwork > 15)
            throw new Error(`The alert came ${result.shownSecondsAfterNetwork} s after the network was back`);
        }
        results[name] = result;
        console.log(name, result);
      }
    } finally {
      wake();
      network(true);
      if (running) stopTask(running);
    }
    // A run of some cases keeps the others' earlier results.
    const file = path.join(evidence, "closed-lifecycle.json");
    const kept = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : {};
    record("closed-lifecycle", { ...kept, ...results });
  } else if (process.argv[2] === "image-share") {
    // Android Files supplies the real content URI grant. A shell-created
    // MediaStore URI alone cannot grant the recipient access to an image.
    adb("shell", "screencap", "-p", "/sdcard/Download/phone-share.png");
    adb("shell", "am", "force-stop", "com.google.android.documentsui");
    adb(
      "shell",
      "am",
      "start",
      "-n",
      "com.google.android.documentsui/com.android.documentsui.files.FilesActivity",
    );
    await pause(1000);
    adb("shell", "uiautomator", "dump", "/sdcard/fixture-ui.xml");
    const xml = adb("exec-out", "cat", "/sdcard/fixture-ui.xml").toString();
    const node = [...xml.matchAll(/<node\s[^>]*>/g)]
      .map((m) => m[0])
      .find((n) => n.includes('text="phone-share.png"'));
    const bounds = node?.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/);
    if (!bounds)
      throw new Error("Open Downloads in Android Files before image-share");
    const x = String(((+bounds[1] + +bounds[3]) / 2) | 0),
      y = String(((+bounds[2] + +bounds[4]) / 2) | 0);
    adb("shell", "input", "swipe", x, y, x, y, "800");
    tap("com.google.android.documentsui:id/action_menu_share");
    await pause(800);
    tap(appLabel);
    await pause(1000);
    await evaluate("e2eApp.ui.reset('work',[{screen:'inbox'}])");
    await pause(700);
    const preview = await evaluate(
      "!!document.querySelector('.work-inbox-picture')",
    );
    if (!preview) throw new Error("Shared image preview missing");
    shot("phone-shared-image");
    await evaluate(
      "[...document.querySelectorAll('.tab:not(.is-hidden) .work-inbox-list li')].find(li=>li.textContent.includes('phone-share.png')).querySelectorAll('button')[1].click()",
    );
    await pause(300);
    await evaluate(
      "[...document.querySelectorAll('dialog[open] button')].find(b=>b.textContent.includes('Phone approvals')).click()",
    );
    await pause(700);
    const imageDraft = await evaluate(
      "(()=>{const p=Object.values(e2eApp.session.state.get().projects).find(p=>p.name==='Phone approvals');return !!e2eApp.ui.draft(p.project_id,null)?.annotationId;})()",
    );
    if (!imageDraft) throw new Error("Image absent from Talk draft");
    if ((await evaluate("e2eApp.ui.route().screen")) !== "talk")
      throw new Error("Shared draft is obscured by another Talk route");
    shot("phone-image-draft");
    fs.writeFileSync(
      path.join(evidence, "image-share.json"),
      JSON.stringify({ preview, imageDraft }),
    );
    console.log({ preview, imageDraft });
  } else if (process.argv[2] === "library-uncertain") {
    fs.writeFileSync(
      path.join(fixture, "notes", "uncertain.md"),
      "# Before disconnect\n",
    );
    await openNote("uncertain.md");
    await click("Edit");
    await replaceNote("# Recovered phone save\n");
    await click("Review changes");
    const pid = companionPid();
    // Freeze only this fixture's companion, dispatch the real save, then take
    // the phone off the network before any acknowledgement can arrive.
    process.kill(pid, "SIGSTOP");
    try {
      await click("Save on computer");
      network(false);
      await waitFor(
        "document.body.innerText.includes('The computer hasn’t confirmed this save')",
      );
      shot("phone-library-uncertain");
    } finally {
      process.kill(pid, "SIGCONT");
      network(true);
    }
    await networkSettled();
    // Reopening the packaged page verifies that the uncertain draft survives
    // the app session; the app reconnects by itself.
    await c.send("Page.reload", {});
    await pause(1300);
    await bindApp();
    await waitFor("e2eApp.session.online", 60);
    await waitFor(
      "document.body.innerText.includes('The computer hasn’t confirmed this save')",
    );
    await click("Check again");
    await waitFor("document.body.innerText.includes('Saved on computer')");
    const saved =
      fs.readFileSync(path.join(fixture, "notes", "uncertain.md"), "utf8") ===
      "# Recovered phone save\n";
    if (!saved)
      throw new Error(
        "Save did not reach the companion; inspect preserved draft",
      );
    shot("phone-library-recovered");
    record("library-uncertain", {
      connectionDropped: true,
      unknownShown: true,
      keptAcrossReload: true,
      recovered: true,
      saved,
    });
  } else if (process.argv[2] === "device-grants") {
    await evaluate("e2eApp.ui.reset('work',[{screen:'connections'}])");
    await pause(800);
    await click("Permissions for Build phone");
    await evaluate(
      "[...document.querySelectorAll('dialog[open] label')].find(e=>e.textContent==='Terminals').querySelector('input').click()",
    );
    shot("phone-device-permissions");
    await click("Save permissions");
    await waitFor(
      "e2eApp.session.online && e2eApp.session.state.get().grants?.terminal===false",
    );
    await evaluate("e2eApp.ui.reset('control')");
    await pause(800);
    await waitFor(
      "document.body.innerText.includes('This device may not open terminals')",
    );
    await evaluate(
      "document.querySelector('#control-terminals').scrollIntoView({block:'center'})",
    );
    shot("phone-terminal-refused");
    await evaluate("e2eApp.ui.reset('work',[{screen:'connections'}])");
    await pause(700);
    await click("Permissions for Build phone");
    await evaluate(
      "[...document.querySelectorAll('dialog[open] label')].find(e=>e.textContent==='Terminals').querySelector('input').click()",
    );
    await click("Save permissions");
    await waitFor(
      "e2eApp.session.online && e2eApp.session.state.get().grants?.terminal===true",
    );
    await pause(800);
    await click("Permissions for Build phone");
    await evaluate(
      "[...document.querySelectorAll('dialog[open] label')].find(e=>e.textContent==='Manage access').querySelector('input').click()",
    );
    await click("Save permissions");
    await waitFor(
      "e2eApp.session.online && e2eApp.session.state.get().grants?.admin===false",
    );
    await waitFor(
      "document.body.innerText.includes('This device may not manage access')",
    );
    await evaluate(
      "document.querySelector('.tab:not(.is-hidden) .notice--warn')?.scrollIntoView({block:'center'})",
    );
    shot("phone-management-refused");
    const id = await evaluate("e2eApp.profile.deviceId");
    // Once this device removes its own management right, only the isolated
    // computer owner may restore it. This is fixture cleanup, not a UI bypass.
    execFileSync(
      path.join(lane, "client/src-tauri/tools/emulator/isolated-companion.sh"),
      [
        path.join(lane, "target/debug/ophio"),
        "devices",
        "allow",
        id,
        "manage",
      ],
      { stdio: "pipe" },
    );
    await waitFor(
      "e2eApp.session.online && e2eApp.session.state.get().grants?.admin===true",
    );
    record("device-grants", {
      changedOwnGrant: true,
      reconnected: true,
      refusalWording: true,
      managementRefused: true,
      ownerRestoredManagement: true,
      restored: true,
    });
  } else if (process.argv[2] === "readiness") {
    await evaluate("e2eApp.ui.reset('work',[{screen:'connections'}])");
    await pause(600);
    await evaluate(
      "document.querySelector('#settings-readiness').scrollIntoView({block:'center'})",
    );
    await click("Check again");
    await waitFor(
      "Object.values(e2eApp.session.commands.get()).filter(c=>c.body.command==='run_readiness_check').some(c=>e2eApp.session.state.get().commandStages[c.id]?.stage==='observed')",
    );
    shot("phone-readiness-checked");
    await click("Open connection details");
    await click("Run checks");
    await waitFor(
      "document.querySelector('dialog[open]')?.innerText.includes('Checks updated.')",
    );
    await evaluate(
      "[...document.querySelectorAll('dialog[open] button')].find(b=>b.textContent==='Run checks').scrollIntoView({block:'center'})",
    );
    shot("phone-readiness-sheet");
    record("readiness", { observed: true, sheetUpdated: true });
    await press('dialog[open] button[aria-label="Close"]');
  } else if (process.argv[2] === "terminal-ui") {
    await evaluate("e2eApp.ui.reset('control')");
    await pause(700);
    await click("New");
    await click("Open new terminal");
    await waitFor(
      "!!document.querySelector('.terminal-composer textarea:not(:disabled)')",
    );
    await fill(".terminal-composer textarea", "printf 'KEY_%s\\n' ROW_OK");
    await press('button[aria-label="Send line"], button[type="submit"]');
    await waitFor(
      "document.querySelector('.xterm-accessibility')?.textContent.includes('KEY_ROW_OK')",
    );
    shot("phone-terminal-text");
    // A tiny authored PTY reader reports bytes, so arrows are verified at the
    // real shell rather than merely checking the labels or command echo.
    const reader =
      "import os,tty,termios; f=0; old=termios.tcgetattr(f); tty.setraw(f); b=b''.join(os.read(f,3) for _ in range(4)); termios.tcsetattr(f,termios.TCSANOW,old); print('ARROWS_'+b.hex(),flush=True)";
    await fill(
      ".terminal-composer textarea",
      `python3 -c '${reader.replaceAll("'", `'"'"'`)}'`,
    );
    await press('button[aria-label="Send line"], button[type="submit"]');
    // While the command field has focus a short screen gives the output its
    // room; leaving the field brings the mode switch back. A narrow phone
    // names the modes briefly, so Keys is chosen by its place.
    await evaluate("document.activeElement?.blur()");
    await waitFor("!!document.querySelector('.terminal-modebar')");
    await press(
      '[role="radiogroup"][aria-label="Terminal input mode"] button:last-child',
    );
    for (const label of ["Left arrow", "Up arrow", "Down arrow", "Right arrow"])
      await press(`button[aria-label="${label}"]`);
    await waitFor(
      "document.querySelector('.xterm-accessibility')?.textContent.includes('ARROWS_1b5b441b5b411b5b421b5b43')",
    );
    shot("phone-terminal-keys");
    // Reaching the arrows scrolled the key row; the toggle leads it.
    await evaluate("document.querySelector('.terminal-keys').scrollLeft = 0");
    // The emulator has a hardware keyboard, and Gboard can be left showing
    // only its small bar for one, which never resizes the page. Clearing its
    // data brings the on-screen keyboard back.
    adb("shell", "pm", "clear", "com.google.android.inputmethod.latin");
    await pause(2500);
    // Clearing restarts the keyboard app. A tap before Android has bound it
    // again asks for a keyboard that is not there yet and is lost, so the tap
    // waits for the binding, and is made once.
    const keyboard = () => adb("shell", "dumpsys", "input_method").toString();
    const keyboardRunning = () => {
      try {
        adb("shell", "pidof", "com.google.android.inputmethod.latin");
        return true;
      } catch {
        return false;
      }
    };
    const bound = () =>
      keyboardRunning() && keyboard().includes("mBoundToMethod=true");
    for (let i = 0; i < 60 && !bound(); i++) await pause(250);
    tap("Show keyboard");
    for (let i = 0; i < 40 && !keyboard().includes("mInputShown=true"); i++)
      await pause(250);
    await pause(700);
    shot("phone-terminal-focused");
    const layout = await evaluate(
      "({width:innerWidth,height:innerHeight,overflow:document.documentElement.scrollWidth>innerWidth,keys:[...document.querySelectorAll('.terminal-keys button')].map(b=>({name:b.getAttribute('aria-label')??b.textContent,height:b.getBoundingClientRect().height}))})",
    );
    const failed = [
      layout.overflow && "the page scrolls sideways",
      layout.keys.length !== 11 && `${layout.keys.length} keys, not 11`,
      layout.keys.some((key) => key.height < 48) && "a key under 48 px",
      !layout.keys.some((key) => key.name === "Hide keyboard") &&
        "the keyboard is not showing",
    ].filter(Boolean);
    if (failed.length)
      throw new Error(
        `Terminal key row or keyboard layout failed: ${failed.join("; ")}`,
      );
    adb("shell", "input", "keyevent", "KEYCODE_BACK");
    record("terminal-ui", { arrowsReceived: true, ...layout });
  } else if (process.argv[2] === "stop-continue") {
    await evaluate(
      `(()=>{const t=Object.values(e2eApp.session.state.get().tasks).find(t=>t.title==='Streaming fixture');window.e2eStopped=t.task_id;e2eApp.ui.selectTask(t.project_id,t.task_id);e2eApp.ui.reset('talk');})()`,
    );
    await waitFor(
      "document.body.innerText.includes('The agent confirmed Stop')",
    );
    const stopped = await evaluate(
      `(()=>{const s=e2eApp.session.state.get(),t=s.tasks[window.e2eStopped],r=s.runs[t.current_run];return {task:t.state,run:r.state,finishedClaim:document.querySelector('.tab:not(.is-hidden)')?.innerText.includes('Finished')};})()`,
    );
    if (
      stopped.task !== "interrupted" ||
      stopped.run !== "interrupted" ||
      stopped.finishedClaim
    )
      throw new Error("Stop claimed completion");
    await evaluate(
      "[...document.querySelectorAll('p')].find(e=>e.textContent.includes('The agent confirmed Stop'))?.scrollIntoView({block:'center'})",
    );
    shot("phone-stop-conversation");
    await click("What happened");
    await waitFor("document.body.innerText.includes('Stop receipt')");
    await evaluate(
      "[...document.querySelectorAll('h2,h3')].find(e=>e.textContent==='Stop receipt')?.scrollIntoView({block:'center'})",
    );
    shot("phone-stop-receipt");
    await evaluate(
      `(()=>{const t=Object.values(e2eApp.session.state.get().tasks).find(t=>t.title==='Live stream fixture');window.e2eContinue=t.task_id;window.e2eFirstRun=t.current_run;e2eApp.ui.selectTask(t.project_id,t.task_id);e2eApp.ui.reset('talk');})()`,
    );
    await pause(500);
    await fill(".composer textarea", "later");
    await click("Continue");
    await waitFor(
      "e2eApp.session.state.get().tasks[window.e2eContinue]?.current_run!==window.e2eFirstRun && e2eApp.session.state.get().tasks[window.e2eContinue]?.state==='completed'",
    );
    await waitFor("document.body.innerText.includes('Continued · run 2')");
    await evaluate(
      "[...document.querySelectorAll('.run-divider')].find(e=>e.textContent.includes('Continued'))?.scrollIntoView({block:'center'})",
    );
    shot("phone-continued");
    await evaluate(
      "[...document.querySelectorAll('button')].filter(b=>b.getClientRects().length&&b.textContent==='What happened').at(-1).click()",
    );
    await pause(500);
    await press('button[aria-label^="Choose a run:"]');
    shot("phone-run-picker");
    await evaluate(
      "[...document.querySelectorAll('dialog[open] button')].find(b=>b.textContent.startsWith('Run 1 ·')).click()",
    );
    await waitFor(
      "!!document.querySelector('button[aria-label^=\"Choose a run: showing Run 1,\"]')",
    );
    shot("phone-earlier-receipt");
    record("stop-continue", {
      ...stopped,
      stopConversation: true,
      stopReceipt: true,
      continuedDivider: true,
      earlierRunSelected: true,
    });
  } else if (process.argv[2] === "library-retained") {
    const note = path.join(fixture, "notes", "retained.md");
    const phoneText = "# Shared plan\n\nEdited on the phone.\n";
    const computerText = "# Shared plan\n\nEdited on the computer meanwhile.\n";
    fs.writeFileSync(note, "# Shared plan\n\nFirst draft.\n");
    await openNote("retained.md");
    await click("Edit");
    await replaceNote(phoneText);
    // An editor on the computer holds the note open while the phone saves.
    const editor = fs.openSync(note, "r+");
    try {
      await click("Review changes");
      await click("Save on computer");
      await waitFor("document.body.innerText.includes('Saved on computer')");
      if (fs.readFileSync(note, "utf8") !== phoneText)
        throw new Error("The phone's save did not reach the computer");
      shot("phone-retained-saved");
      // Its write lands in the version the phone's save replaced, so it no
      // longer shows in the note.
      fs.ftruncateSync(editor, 0);
      fs.writeSync(editor, computerText, 0);
      fs.fsyncSync(editor);
    } finally {
      fs.closeSync(editor);
    }
    // Each save keeps its replaced version as a recovery record for a while.
    const recoveries = path.join(fixture, "data", "ophio", "library", "recovery");
    const recoveryId = fs
      .readdirSync(recoveries)
      .find(
        (id) =>
          JSON.parse(
            fs.readFileSync(path.join(recoveries, id, "record.json"), "utf8"),
          ).path === "retained.md",
      );
    // The computer judges a replaced version once its five-minute grace for
    // late writes has passed, and keeps one that changed.
    await pause(5 * 60 * 1000);
    const heading =
      "[...document.querySelectorAll('.tab:not(.is-hidden) h2')].some(h=>h.textContent==='Needs checking')";
    const root =
      "(()=>{const r=e2eApp.session.state.get().libraryRoots.find(r=>r.label==='Notes');return r.root_id;})()";
    for (let n = 0; !(await evaluate(heading)); n++) {
      if (n === 60) throw new Error("The retained copy was never listed");
      await evaluate("e2eApp.ui.reset('library')");
      await pause(2000);
    }
    await waitFor(
      "document.body.innerText.includes('The file changed on the computer while it was being saved')",
    );
    await evaluate(
      "[...document.querySelectorAll('.tab:not(.is-hidden) h2')].find(h=>h.textContent==='Needs checking').scrollIntoView({block:'center'})",
    );
    shot("phone-retained-listed");
    await evaluate(
      "[...document.querySelectorAll('.tab:not(.is-hidden) button')].find(b=>b.textContent.startsWith('retained.md')).click()",
    );
    await waitFor("document.body.innerText.includes('Version to keep')");
    await evaluate(
      "[...document.querySelectorAll('.lib-recovery-choices label')].find(l=>l.textContent==='Displaced version').querySelector('input').click()",
    );
    await waitFor(
      "document.body.innerText.includes('Keep “Displaced version” · Review save')",
    );
    shot("phone-retained-compare");
    // Keep the computer's lost edit, then write both into one version.
    await click("Keep “Displaced version” · Review save");
    await waitFor("!!document.querySelector('.cm-content')");
    if (
      !(await evaluate(
        `document.querySelector('.cm-content').innerText.includes(${JSON.stringify(computerText.trim().split("\n").at(-1))})`,
      ))
    )
      throw new Error("The recovery draft is not the displaced version");
    const merged =
      "# Shared plan\n\nEdited on the phone.\nEdited on the computer meanwhile.\n";
    await replaceNote(merged);
    await click("Review changes");
    await click("Save on computer");
    await waitFor("document.body.innerText.includes('Saved on computer')");
    if (fs.readFileSync(note, "utf8") !== merged)
      throw new Error("The resolved note is not on the computer");
    await evaluate(
      `e2eApp.ui.push({screen:'library_recovery',rootId:${root},recoveryId:${JSON.stringify(recoveryId)}})`,
    );
    await waitFor("document.body.innerText.includes('Discard this recovery copy…')");
    await click("Discard this recovery copy…");
    await click("Discard the recovery copy of retained.md");
    await waitFor("document.body.innerText.includes('Recovery copy discarded')");
    shot("phone-retained-resolved");
    const leftovers = fs
      .readdirSync(path.join(fixture, "notes"))
      .filter((f) => f.startsWith(`.ophio-save-${recoveryId}`));
    const kept = fs.existsSync(path.join(recoveries, recoveryId));
    if (leftovers.length || kept)
      throw new Error("The discarded recovery copy is still on the computer");
    await evaluate("e2eApp.ui.reset('library')");
    await pause(1500);
    if (await evaluate(heading))
      throw new Error("Needs checking still lists the resolved copy");
    record("library-retained", {
      phoneSaved: true,
      computerWriteDisplaced: true,
      listedAsNeedsChecking: true,
      reason: "The file changed on the computer while it was being saved",
      draftFromDisplacedVersion: true,
      noteOnComputer: merged,
      recoveryCopyDiscarded: true,
      nothingLeftBeside: true,
    });
  } else if (process.argv[2] === "transfer-quota") {
    // Two large uploads this phone declared and has not finished hold the
    // device's whole allowance: twice the computer's 2 GiB single-file limit.
    const held = ["held-1.bin", "held-2.bin"];
    const heldIds = [];
    for (const name of held)
      heldIds.push(
        await evaluate(`(async()=>{
          const s=e2eApp.session;
          const r=await s.command({command:'create_upload',name:${JSON.stringify(name)},size:2*1024**3,sha256:'0'.repeat(64),destination:{kind:'inbox'}},{label:'Declare ${name}'});
          for(let i=0;i<200;i++){const result=r.result??(await s.query({kind:'command_status',command_id:r.command_id})).result;if(result)return result.transfer.transfer_id;await new Promise(r=>setTimeout(r,40));}
          throw new Error('Declaration not observed');
        })()`),
      );
    const heldStates = () =>
      evaluate(
        `${JSON.stringify(heldIds)}.map(id=>e2eApp.session.state.get().transfers[id]?.state.state)`,
      );
    const name = "quota-note.txt";
    adb("shell", `printf 'Quota fixture\\n' > /sdcard/Download/${name}`);
    await chooseFile(name);
    await press(".transfer-footer button");
    await waitFor(rowSays(name, "Needs attention"));
    const said = await evaluate(
      `${transferRow(name)}.querySelector('.notice')?.innerText??''`,
    );
    if (
      !/This device already has 4(\.0)? GB of files on their way to the computer/.test(
        said,
      )
    )
      throw new Error(`Quota message missing: ${said}`);
    await evaluate(`${transferRow(name)}.scrollIntoView({block:'center'})`);
    shot("phone-transfer-quota");
    const refused = {
      recordsForRefusedFile: (await records(name)).length,
      inInbox: fs.existsSync(
        path.join(fixture, "home", "Downloads", "ophio", name),
      ),
      held: await heldStates(),
    };
    if (
      refused.recordsForRefusedFile ||
      refused.inInbox ||
      refused.held.some((state) => state !== "pending")
    )
      throw new Error("The computer's records disagree with the refusal");
    // Cancelling the held uploads frees the allowance; the same file then goes.
    for (const heldName of held) {
      await click(`Cancel ${heldName}`);
      await waitFor(rowSays(heldName, "Cancelled"));
    }
    await click(`Cancel ${name}`);
    await waitFor(rowSays(name, "Cancelled"));
    await chooseFile(name);
    await press(".transfer-footer button");
    await waitFor(rowSays(name, "Completed"), 60);
    const arrived = path.join(fixture, "home", "Downloads", "ophio", name);
    if (fs.readFileSync(arrived, "utf8") !== "Quota fixture\n")
      throw new Error("The file did not arrive after the allowance freed");
    await evaluate(`${transferRow(name)}.scrollIntoView({block:'center'})`);
    shot("phone-transfer-quota-freed");
    record("transfer-quota", {
      message: said,
      ...refused,
      heldAfterCancel: await heldStates(),
      sentAfterFreeing: true,
    });
  } else if (process.argv[2] === "upload-resume") {
    const name = "resume-upload.bin";
    adb(
      "shell",
      `dd if=/dev/urandom of=/sdcard/Download/${name} bs=1048576 count=64 2>/dev/null`,
    );
    const sent = adb("shell", "sha256sum", `/sdcard/Download/${name}`)
      .toString()
      .split(/\s/)[0];
    const earlier = new Set((await records(name)).map((r) => r.id));
    const ours = async () =>
      (await records(name)).filter((r) => !earlier.has(r.id));
    await chooseFile(name);
    await press(".transfer-footer button");
    await waitFor(`${progressOf(name)}>=8*1024*1024`, 60);
    // Leave coverage part way through, then put the app away.
    network(false);
    let id, staged;
    try {
      await waitFor(rowSays(name, "Continues when connected"));
      shot("phone-upload-dropped");
      [{ id }] = await ours();
      staged = fs.statSync(
        path.join(fixture, "data", "ophio", "db", "staging", id),
      ).size;
      adb("shell", "input", "keyevent", "KEYCODE_HOME");
      await pause(1000);
    } finally {
      network(true);
    }
    await networkSettled();
    adb("shell", "am", "start", "-n", "app.ophio/.MainActivity");
    // It continues from what the computer already holds, by itself.
    const stagingFile = path.join(fixture, "data", "ophio", "db", "staging", id);
    let lowest = Infinity;
    for (let n = 0; !(await evaluate(rowSays(name, "Completed"))); n++) {
      if (n === 1800) throw new Error("The upload did not continue by itself");
      if (fs.existsSync(stagingFile))
        lowest = Math.min(lowest, fs.statSync(stagingFile).size);
      await pause(100);
    }
    await evaluate(`${transferRow(name)}.scrollIntoView({block:'center'})`);
    shot("phone-upload-resumed");
    const arrived = path.join(fixture, "home", "Downloads", "ophio", name);
    const result = {
      droppedAt: staged,
      lowestStagedAfterReturn: lowest,
      transfers: (await ours()).length,
      sha256Match: sha256(arrived) === sent,
      stagingRemoved: !fs.existsSync(stagingFile),
    };
    if (
      !(staged > 0 && staged < 64 * 1024 * 1024) ||
      lowest < staged ||
      result.transfers !== 1 ||
      !result.sha256Match ||
      !result.stagingRemoved
    )
      throw new Error(`Upload did not resume intact: ${JSON.stringify(result)}`);
    record("upload-resume", result);
  } else if (process.argv[2] === "download-cancel") {
    const name = "cancel-download.bin";
    const source = path.join(fixture, "notes", name);
    fs.writeFileSync(source, randomBytes(96 * 1024 * 1024));
    const before = sha256(source);
    await openNote(name);
    await click("Send to this device");
    await waitFor(`${progressOf(name)}>=16*1024*1024`, 60);
    shot("phone-download-sending");
    await click(`Cancel ${name}`);
    await waitFor(rowSays(name, "Cancelled"));
    await evaluate(`${transferRow(name)}.scrollIntoView({block:'center'})`);
    shot("phone-download-cancelled");
    const [held] = await records(name);
    const result = {
      computerState: held.state,
      sentBeforeCancel: held.transferred,
      size: held.size,
      sourceUnchanged: sha256(source) === before,
      savedOnPhone: onPhone(name),
      saveOffered: await evaluate(
        `[...${transferRow(name)}.querySelectorAll('button')].some(b=>b.textContent.startsWith('Save'))`,
      ),
    };
    if (
      result.computerState !== "cancelled" ||
      !(result.sentBeforeCancel < result.size) ||
      !result.sourceUnchanged ||
      result.savedOnPhone ||
      result.saveOffered
    )
      throw new Error(`Cancelled download left state: ${JSON.stringify(result)}`);
    record("download-cancel", result);
  } else if (process.argv[2] === "large-text") {
    // Clipped text, sideways overflow, and controls that are covered, cannot
    // be scrolled to, or leave a finger less than 44px to press, on the
    // screen now shown, or only among the controls in `within`.
    const audit = (within = "body") =>
      evaluate(`(()=>{
        // Closed details keep layout boxes; screen-reader-only text is 1 px.
        const shown=e=>{const r=e.getBoundingClientRect(),d=e.closest('details:not([open])');return r.width>1&&r.height>1&&getComputedStyle(e).visibility!=='hidden'&&!e.closest('.is-hidden,[inert],[aria-hidden=true]')&&(!d||d.querySelector(':scope>summary')?.contains(e));};
        const name=e=>(e.getAttribute('aria-label')||e.textContent||e.getAttribute('placeholder')||e.tagName).trim().replace(/\\s+/g,' ').slice(0,70);
        const clipped=[...document.querySelectorAll('body *')].filter(e=>shown(e)&&e.textContent.trim()&&!['INPUT','TEXTAREA'].includes(e.tagName)).filter(e=>{const s=getComputedStyle(e);const x=['hidden','clip'].includes(s.overflowX),y=['hidden','clip'].includes(s.overflowY);return (x&&e.scrollWidth>e.clientWidth+1)||(y&&e.scrollHeight>e.clientHeight+1);}).filter((e,_,all)=>!all.some(o=>o!==e&&e.contains(o))).map(name);
        const controls=[...document.querySelector(${JSON.stringify(within)}).querySelectorAll('button,a[href],input:not([type=hidden]),textarea,select,summary,[role=button],[role=tab],[role=radio]')].filter(shown);
        const small=[],covered=[];
        // What a press at (x, y) reaches: the control, a hit area it draws
        // beyond its box, or a label that is part of it.
        const at=(x,y)=>x>=0&&y>=0&&x<innerWidth&&y<innerHeight?document.elementFromPoint(x,y):null;
        const reaches=(e,hit)=>!!hit&&(e===hit||e.contains(hit)||e.labels&&[...e.labels].some(l=>l.contains(hit)));
        // How far a finger can land from the centre and still press it, out
        // to 40px each way. Only this counts: a box that is clipped or
        // covered at its edges is not all there to press.
        const span=(e,x,y,dx,dy)=>{let n=0;while(n<40&&reaches(e,at(x+dx*(n+1),y+dy*(n+1))))n++;return n;};
        for(const e of controls){
          e.scrollIntoView({block:'center',inline:'center'});
          const r=e.getBoundingClientRect(),x=r.left+r.width/2,y=r.top+r.height/2;
          const hit=at(x,y);
          if(!hit||!(reaches(e,hit)||hit.contains(e)))covered.push(name(e)+(hit?' under '+name(hit):' off screen'));
          else if(!e.closest('p,li>span')){
            const w=1+span(e,x,y,-1,0)+span(e,x,y,1,0),h=1+span(e,x,y,0,-1)+span(e,x,y,0,1);
            if(Math.min(w,h)<44)small.push(name(e)+' '+Math.round(w)+'×'+Math.round(h));
          }
        }
        scrollTo(0,0);document.querySelectorAll('.screen__body,.tab:not(.is-hidden) *').forEach(e=>{if(e.scrollTop)e.scrollTop=0;});
        return {route:e2eApp.ui.route().screen,bodyFont:getComputedStyle(document.body).fontSize,sideways:document.documentElement.scrollWidth>innerWidth,clipped,small,covered};
      })()`);
    // The audit first proves itself on controls it must fail: 48 px boxes
    // clipped or covered to a 20 px strip, beside one left whole.
    await evaluate(`(()=>{
      const f=document.createElement('div');f.id='e2e-reach';
      f.style.cssText='position:fixed;left:8px;top:8px;z-index:2147483647;display:flex;gap:16px;padding:8px;background:#fff';
      const button=label=>{const b=document.createElement('button');b.textContent=label;b.style.cssText='width:48px;height:48px;margin:0;padding:0;border:0;font-size:8px';return b;};
      const clip=document.createElement('div');clip.style.cssText='width:20px;height:48px;overflow:hidden;display:flex;justify-content:center';
      const clipped=button('clipped');clipped.style.flex='none';clip.append(clipped);
      const cover=document.createElement('div');cover.style.cssText='position:relative;width:48px;height:48px';
      const covered=button('covered');cover.append(covered);
      for(const side of ['left','right']){const c=document.createElement('div');c.style.cssText='position:absolute;top:0;'+side+':0;width:14px;height:48px;background:#888';cover.append(c);}
      f.append(clip,cover,button('whole'));document.body.append(f);
    })()`);
    const proof = await audit("#e2e-reach");
    await evaluate("document.getElementById('e2e-reach').remove()");
    // An open sheet makes the rest of the page inert, the fixture too.
    if (proof.covered.length)
      throw new Error(`Something the app shows covers the reach fixture: ${proof.covered.join("; ")}`);
    const short = proof.small.map((s) => s.split(" ")[0]).sort();
    if (short.join() !== "clipped,covered")
      throw new Error(`The reach audit missed clipped or covered edges: ${JSON.stringify(proof)}`);
    const screens = [];
    const check = async (name, open) => {
      await evaluate(open);
      await pause(1200);
      shot(`large-${name}`);
      screens.push({ name, ...(await audit()) });
    };
    // A fresh request, so the journey ends by answering it at this size.
    const task = await evaluate(`(async()=>{
      const s=e2eApp.session,p=Object.values(s.state.get().projects).find(p=>p.name==='Phone approvals');
      const r=await s.command({command:'create_task',project_id:p.project_id,kind:'coding',mode:'work',title:'Approval at large text',prompt:'bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Large text fixture'});
      return r.result.task_id;
    })()`);
    await waitFor(
      `Object.values(e2eApp.session.state.get().approvals).some(a=>a.task_id===${JSON.stringify(task)}&&a.state==='pending')`,
    );
    const font = "parseFloat(getComputedStyle(document.body).fontSize)";
    // A run stopped part way can leave the scale raised; start from the default.
    adb("shell", "settings", "put", "system", "font_scale", "1.0");
    await pause(1500);
    const base = await evaluate(font);
    adb("shell", "settings", "put", "system", "font_scale", "2.0");
    try {
      await waitFor(`${font}>=${2 * base - 1}`);
      await check(
        "talk-approval",
        `e2eApp.ui.selectTask(e2eApp.session.state.get().tasks[${JSON.stringify(task)}].project_id,${JSON.stringify(task)});e2eApp.ui.reset('talk')`,
      );
      await evaluate(
        "[...document.querySelectorAll('.tab:not(.is-hidden) button')].find(b=>b.textContent.trim()==='Allow').scrollIntoView({block:'center'})",
      );
      await pause(500);
      shot("large-talk-allow");
      // A real touch on the control Android reports, as a person would tap.
      tap("Allow");
      await waitFor(
        `Object.values(e2eApp.session.state.get().approvals).every(a=>a.task_id!==${JSON.stringify(task)}||a.state!=='pending')`,
      );
      await check("work", "e2eApp.ui.reset('work')");
      await check("control", "e2eApp.ui.reset('control')");
      await check("library", "e2eApp.ui.reset('library')");
      await check(
        "note",
        "(()=>{const r=e2eApp.session.state.get().libraryRoots.find(r=>r.label==='Notes');e2eApp.ui.reset('library',[{screen:'file',rootId:r.root_id,path:'guide.md'}]);})()",
      );
      await check("transfers", "e2eApp.ui.reset('library',[{screen:'transfers'}])");
      await check("connections", "e2eApp.ui.reset('work',[{screen:'connections'}])");
      await check("settings", "e2eApp.ui.reset('work',[{screen:'settings'}])");
    } finally {
      adb("shell", "settings", "put", "system", "font_scale", "1.0");
    }
    record("large-text", { fontScale: 2, approvedByTap: true, auditProof: proof.small, screens });
    // Clipped text is recorded, not failed: a header shortens a long project
    // name on purpose.
    const faults = screens.filter(
      (s) => s.sideways || s.small.length || s.covered.length,
    );
    if (faults.length)
      throw new Error(`Large text breaks controls: ${JSON.stringify(faults)}`);
  } else if (process.argv[2] === "revoke") {
    const id = await evaluate("e2eApp.profile.deviceId");
    execFileSync(
      path.join(lane, "client/src-tauri/tools/emulator/isolated-companion.sh"),
      [path.join(lane, "target/debug/ophio"), "devices", "revoke", id],
      { stdio: "pipe" },
    );
    await pause(1500);
    const revoked = await evaluate(
      "e2eApp.session.link.get().state==='failed'&&e2eApp.session.link.get().reason==='revoked'",
    );
    await evaluate("e2eApp.session.reconnect()");
    await pause(1500);
    const cannotReconnect = await evaluate(
      "!e2eApp.session.online&&e2eApp.session.link.get().state==='failed'",
    );
    if (!revoked || !cannotReconnect)
      throw new Error("Revoked phone reconnected or wrong terminal state");
    await evaluate("e2eApp.ui.reset('work',[{screen:'connections'}])");
    await waitFor(
      "document.body.innerText.includes('This device’s access was revoked')",
    );
    await evaluate(
      "[...document.querySelectorAll('p')].find(e=>e.textContent==='This device’s access was revoked').scrollIntoView({block:'center'})",
    );
    await pause(350);
    shot("phone-revoked");
    fs.writeFileSync(
      path.join(evidence, "revoke.json"),
      JSON.stringify({ revoked, cannotReconnect }),
    );
    console.log({ revoked, cannotReconnect });
  } else if (process.argv[2] === "text")
    console.log(await evaluate("document.body.innerText"));
} finally {
  if (wasExempt !== null) {
    try {
      unrestricted(wasExempt);
    } catch (e) {
      console.error(`Battery use wasn't put back: ${e.message}`);
    }
  }
  c.close();
}
