// SPDX-License-Identifier: Apache-2.0
// Isolated emulator only. Fixture pairing proof is consumed without logging it.
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { connect, pause } from "./cdp.mjs";
import { adb, tap } from "./adb.mjs";
const repo = fileURLToPath(new URL("../../../../", import.meta.url));
const port = Number(
  fs.readFileSync(`${repo}/target/preview-e2e/cdp-port`, "utf8").trim(),
);
const c = await connect(port);
const wait = async (expression) => {
  for (let n = 0; n < 120; n++) {
    if (await c.evaluate(expression)) return;
    await pause(150);
  }
  throw new Error("App state did not arrive");
};
try {
  if (process.argv[2] === "pair") {
    const link = fs
      .readFileSync(`${repo}/target/phone-e2e/pair.txt`, "utf8")
      .match(/ophio:\/\/pair\?d=([\w-]+)/);
    if (!link) throw new Error("Fixture offer unavailable");
    const offer = JSON.parse(Buffer.from(link[1], "base64url"));
    offer.addresses = [`10.0.2.2:${process.env.PREVIEW_OPHIO_PORT}`];
    // The computer holds the request until `ophio pair confirm` answers it
    // with the code the phone was given.
    await c.evaluate(
      `(()=>{const t=window.__TAURI_INTERNALS__;const waiting='__CHANNEL__:'+t.transformCallback(m=>{if('message' in m)window.previewPairCode=m.message;});window.previewPaired=t.invoke('pair',{offer:${JSON.stringify(offer)},deviceName:'Preview test phone',replace:false,waiting}).then(()=>'paired',e=>e?.message??'failed');window.previewPaired.then(r=>{window.previewPairing=r;});return true;})()`,
    );
    await wait("window.previewPairCode");
    const code = await c.evaluate("window.previewPairCode");
    fs.writeFileSync(`${repo}/target/phone-e2e/pair-code.txt`, code, {
      mode: 0o600,
    });
    console.log(`The phone shows ${code}; confirm it with ophio pair confirm.`);
    for (let n = 0; !(await c.evaluate("window.previewPairing")); n++) {
      if (n === 1500) throw new Error("Nobody confirmed the pairing");
      await pause(200);
    }
    if ((await c.evaluate("window.previewPairing")) !== "paired")
      throw new Error("Pairing did not complete");
    await c.evaluate("location.reload()");
    await pause(1200);
    console.log("ok: fresh fixture pairing over pinned TLS");
  } else if (process.argv[2] === "open") {
    await c.evaluate(
      `(()=>{const el=document.querySelector('.app');let f=el[Object.keys(el).find(k=>k.startsWith('__reactFiber'))];for(;f;f=f.return){if(f.memoizedProps?.app){window.previewApp=f.memoizedProps.app;return;}}throw new Error('Connected app unavailable')})()`,
    );
    await wait("previewApp.session.online && previewApp.session.state.get().caughtUp");
    await c.evaluate(
      `(async()=>{const a=previewApp;let project=Object.values(a.session.state.get().projects)[0];if(!project){const r=await a.session.command({command:'add_project',host_path:${JSON.stringify(`${repo}/target/phone-e2e/project`)},name:'Preview fixture'},{label:'Add preview fixture'});project={project_id:r.result.project_id};}a.ui.selectProject(project.project_id);const url=${JSON.stringify(`http://127.0.0.2:5890/@fs${repo}target/preview-fixture/index.html`)};await a.session.command({command:'register_preview',project_id:project.project_id,label:'Vite preview',url},{label:'Register local preview'});a.ui.selectTab('control');})()`,
    );
    await pause(800);
    await c.evaluate(
      `document.querySelector('.preview-card').scrollIntoView()`,
    );
    await c.evaluate(
      `document.querySelector('.preview-card .preview-actions button').click()`,
    );
    await pause(1800);
    console.log("ok: Open tapped on the packaged preview card");
  } else if (process.argv[2] === "recovery") {
    await wait("!!document.querySelector('.preview-draft img')");
    console.log("ok: phone picture survived process restart");
  } else if (process.argv[2] === "capture") {
    const before = await c.evaluate(
      "document.querySelectorAll('.preview-draft img').length",
    );
    tap("TAKE PICTURE");
    await wait(
      `document.querySelectorAll('.preview-draft img').length > ${before}`,
    );
    fs.writeFileSync(
      `${repo}/target/preview-results/picture.png`,
      adb("exec-out", "screencap", "-p"),
    );
    console.log("ok: native preview picture returned and persisted on phone");
  } else if (process.argv[2] === "attach") {
    const linked = await c.evaluate(
      `(async()=>{const a=previewApp,project=Object.values(a.session.state.get().projects)[0];let task=Object.values(a.session.state.get().tasks).find(t=>t.title==='Preview evidence');if(!task){const r=await a.session.command({command:'create_task',project_id:project.project_id,kind:'coding',mode:'work',title:'Preview evidence',prompt:'Review this picture when explicitly started.',bundle:null,harness:'codex',model:null,effort:null,mission:null,target_id:null,placement:{mode:'queue',position:null,authorization:'ask_before_start'}},{label:'Queue preview evidence fixture'});if(r.result?.kind!=='task_created')throw new Error('Queue fixture missing');}return true;})()`,
    );
    if (!linked) throw new Error("Queue-only task creation failed");
    await pause(300);
    await c.evaluate(
      `(()=>{const draft=[...document.querySelectorAll('.preview-draft')].find(d=>[...d.querySelectorAll('button')].some(b=>b.textContent.trim()==='Attach to this task'));if(!draft)throw new Error('Unattached picture missing');const s=draft.querySelector('select');const task=Object.values(previewApp.session.state.get().tasks).find(t=>t.title==='Preview evidence');s.value=task.task_id;s.dispatchEvent(new Event('change',{bubbles:true}));})()`,
    );
    await pause(300);
    const beforeAttached = await c.evaluate(
      "[...document.querySelectorAll('.preview-draft')].filter(d=>d.textContent.includes('Attached as Source')).length",
    );
    await c.evaluate(
      `(()=>{const b=[...document.querySelectorAll('.preview-draft button')].find(b=>b.textContent.trim()==='Attach to this task');if(!b||b.disabled)throw new Error('Attachment unavailable');b.click();})()`,
    );
    await wait(
      `[...document.querySelectorAll('.preview-draft')].filter(d=>d.textContent.includes('Attached as Source')).length > ${beforeAttached}`,
    );
    const result = await c.evaluate(
      `(async()=>{const a=previewApp,task=Object.values(a.session.state.get().tasks).find(t=>t.title==='Preview evidence');const r=await a.session.query({kind:'task_artifacts',task_id:task.task_id,before_artifact_id:null,limit:10});return {queued:task.state,sourceLinked:r.history.artifacts.some(a=>a.links?.some(l=>l.role==='source'&&l.task_id===task.task_id))};})()`,
    );
    if (result.queued !== "accepted" || !result.sourceLinked)
      throw new Error("Source link was not recorded on the queued task");
    console.log(
      "ok: queued task Source link confirmed after verified PNG upload",
    );
  } else if (process.argv[2] === "closed") {
    await c.evaluate("previewApp.session.stop()");
    await wait("document.body.textContent.includes('Preview connection lost')");
    console.log("ok: control loss closed the preview");
  }
} finally {
  c.close();
}
