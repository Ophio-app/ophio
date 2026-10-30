// SPDX-License-Identifier: Apache-2.0
// Drive only the packaged app in the isolated Android emulator WebView.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { connect, pause } from "../../client/src-tauri/tools/emulator/cdp.mjs";
import { guestAddress } from "./offer.mjs";
import { isTaskStop } from "./stop_button.mjs";

for (const key of ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"])
  delete process.env[key];
if (!["tcp:5038", "tcp:5042"].includes(process.env.ADB_SERVER_SOCKET))
  throw new Error("Source the isolated Android build environment first");
const serial = process.env.LAB_ADB_SERIAL ?? "emulator-5562";
const cdpPort = Number(process.env.LAB_CDP_PORT ?? "9226");
// An emulator console port (never a physical device) and a loopback forward.
const consolePort = Number(/^emulator-(\d{4})$/.exec(serial)?.[1]);
if (
  !(consolePort >= 5554 && consolePort <= 5584 && consolePort % 2 === 0) ||
  !(Number.isInteger(cdpPort) && cdpPort >= 1024 && cdpPort <= 65535)
)
  throw new Error("Unexpected isolated emulator endpoint");
const root = fileURLToPath(new URL("../../", import.meta.url));
const [target, step] = process.argv.slice(2);
if (!["hyprland", "gnome", "plasma"].includes(target))
  throw new Error("Expected a lab target");
const runLabel = process.env.LAB_EVIDENCE_RUN;
if (runLabel && !/^[a-zA-Z0-9_-]{1,40}$/.test(runLabel))
  throw new Error("Invalid evidence run label");
const evidence = path.join(
  root,
  "target/lab-evidence",
  ...(runLabel ? [runLabel] : []),
  target,
);
fs.mkdirSync(evidence, { recursive: true, mode: 0o700 });
let c;
for (let attempt = 0; attempt < 20; attempt++) {
  try {
    c = await connect(cdpPort);
    break;
  } catch (error) {
    if (attempt === 19) throw error;
    await pause(500);
  }
}
const evaluate = (expression) => c.evaluate(expression);
const visible = (text) =>
  evaluate(`[...document.querySelectorAll('button')].some(b =>
    b.getClientRects().length && !b.closest('.is-hidden') && !b.disabled && b.textContent.trim() === ${JSON.stringify(text)})`);
const findApp = () =>
  evaluate(`(() => {
    const el = document.querySelector('.app');
    let f = el?.[Object.keys(el).find(k => k.startsWith('__reactFiber'))];
    for (; f; f = f.return) if (f.memoizedProps?.app) { window.labApp = f.memoizedProps.app; return; }
    throw new Error('Connected app unavailable');
  })()`);
const click = async (text) => {
  await evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find(b =>
      b.getClientRects().length && !b.closest('.is-hidden') && b.textContent.trim() === ${JSON.stringify(text)});
    if (!button || button.disabled) throw new Error('Requested button unavailable');
    button.click();
  })()`);
  await pause(500);
};
const fill = async (label, value) =>
  evaluate(`(() => {
  const label = [...document.querySelectorAll('label')].find(l => l.textContent.includes(${JSON.stringify(label)}));
  const el = label?.querySelector('input,textarea') ?? document.getElementById(label?.htmlFor);
  if (!el) throw new Error('Requested field unavailable');
  Object.getOwnPropertyDescriptor(el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, 'value').set.call(el, ${JSON.stringify(value)});
  el.dispatchEvent(new Event('input', { bubbles: true }));
})()`);
const record = (name, value) =>
  fs.writeFileSync(
    path.join(evidence, name + ".json"),
    JSON.stringify(value, null, 2) + "\n",
    { mode: 0o600 },
  );
const screenshot = (name) => {
  fs.writeFileSync(
    path.join(evidence, name + ".png"),
    execFileSync("adb", ["-s", serial, "exec-out", "screencap", "-p"], {
      maxBuffer: 16 * 1024 * 1024,
    }),
  );
};

try {
  if (step === "pair") {
    const file = path.join(root, "target/lab", target, "pair.txt");
    const match = fs
      .readFileSync(file, "utf8")
      .match(/ophio:\/\/pair\?d=([\w-]+)/);
    if (!match) throw new Error("No protected lab pairing offer");
    const offer = JSON.parse(Buffer.from(match[1], "base64url"));
    // The emulator reaches the guest through the host forward it was started with.
    const guest = JSON.parse(
      fs.readFileSync(path.join(root, "target/lab", target, "process.json"), "utf8"),
    );
    offer.addresses = [guestAddress(guest)];
    if (await visible("Pair a computer")) await click("Pair a computer");
    else {
      // A phone that was paired before starts from its computers list.
      await findApp();
      await evaluate("labApp.ui.reset('work',[{screen:'connections'}])");
      await pause(800);
      await click("Pair another computer");
      if (await visible("Pair a computer")) await click("Pair a computer");
    }
    await click("Paste the link instead");
    await fill(
      "Pairing link",
      `ophio://pair?d=${Buffer.from(JSON.stringify(offer)).toString("base64url")}`,
    );
    await click("Continue");
    await fill("Name this phone", "Qualification phone");
    // Pairing a known computer again asks to replace the earlier pairing.
    await evaluate(`(() => {
      const box = [...document.querySelectorAll('label')].find(l => l.textContent.includes('Replace the earlier pairing'))?.querySelector('input');
      if (box && !box.checked) box.click();
    })()`);
    await pause(300);
    await click((await visible("Pair again")) ? "Pair again" : "Pair");
    // The guest holds the request until its owner confirms the code shown.
    const shown = async () =>
      evaluate("document.querySelector('.pair-code')?.textContent ?? ''");
    for (let n = 0; !(await shown()); n++) {
      if (n === 100) throw new Error("The phone did not show a pairing code");
      await pause(200);
    }
    const code = (await shown()).replace(/\s/g, "");
    fs.writeFileSync(path.join(root, "target/lab", target, "pair-code.txt"), code, {
      mode: 0o600,
    });
    console.log(`The phone shows ${code}; confirm it on the guest.`);
    const paired = () =>
      evaluate(
        "[...document.querySelectorAll('button')].some(b => b.textContent.trim() === 'Continue')",
      );
    for (let n = 0; !(await paired()); n++) {
      if (n === 1500) throw new Error("Nobody confirmed the pairing on the guest");
      await pause(200);
    }
    await click("Continue");
    await pause(1500);
    fs.unlinkSync(file);
    screenshot("paired");
    record("pair", {
      status: "pass",
      method: "Welcome UI, native pinned TLS and device authentication",
    });
  } else {
    await findApp();
    if (step === "initial-control") {
      const result = await evaluate(
        "({control:labApp.session.state.get().control,text:document.body.innerText,buttonDisabled:[...document.querySelectorAll('button')].find(b=>b.getClientRects().length && b.textContent.trim()==='Enable control')?.disabled})",
      );
      record("initial-control", result);
      if (result.buttonDisabled !== false)
        throw new Error("Initial control is unavailable");
    } else if (step === "initialize-control") {
      record(
        "initial-control",
        await evaluate(
          "({control:labApp.session.state.get().control,text:document.body.innerText,buttonDisabled:[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Enable control')?.disabled})",
        ),
      );
      // Diagnostic workaround, deliberately separate from the normal UI test.
      // This must be reported as an initial-control failure if it was needed.
      await evaluate(
        "labApp.session.command({command:'pause_input',target_id:labApp.session.state.get().targets[0].target_id},{label:'Lab control initialization workaround'})",
      );
      await pause(500);
    } else if (step === "status") {
      const result = await evaluate(
        `(async () => ({ snapshot: await labApp.session.query({kind:'snapshot'}), text:document.body.innerText }))()`,
      );
      record("phone-status", result);
      console.log(JSON.stringify(result));
    } else if (step === "watch" || step === "open") {
      // A restart may leave the old target visible until journal catch-up.
      // Select a display only from the current, reconciled inventory.
      for (let attempt = 0; attempt < 180; attempt++) {
        if (await evaluate("labApp.session.link.get().state==='online' && labApp.session.state.get().caughtUp")) break;
        await pause(250);
      }
      if (!await evaluate("labApp.session.link.get().state==='online' && labApp.session.state.get().caughtUp"))
        throw Error("The computer has not supplied a current display inventory");
      await evaluate(`(() => {
        window.labMedia = {opened: null, frames: [], controls: [], draws: 0};
        const draw = window.labOriginalDraw ??= CanvasRenderingContext2D.prototype.drawImage;
        CanvasRenderingContext2D.prototype.drawImage = function(...args) { const result = draw.apply(this,args); if (this.canvas.closest('.live-picture')) labMedia.draws++; return result; };
        const media = window.labOriginalMedia ??= labApp.platform.media.bind(labApp.platform);
        labApp.platform.media = (...args) => {
          const link = media(...args), send = link.send.bind(link);
          let received = new Map(), draws = labMedia.draws;
          link.listen(event => {
            if (event.type === 'text') labMedia.controls.push({at:performance.now(), frame:event.frame});
            if (event.data instanceof Uint8Array && event.data.length >= 44) {
              const d = new DataView(event.data.buffer,event.data.byteOffset,event.data.byteLength);
              received.set(Number(d.getBigUint64(8)), {received:performance.now(),revision:Number(d.getBigUint64(16)),width:d.getUint32(28),height:d.getUint32(32)});
            }
          });
          link.send = frame => {
            if (frame.type === 'open') labMedia.opened = performance.now();
            if (frame.type === 'ack') {
              labMedia.frames.push({seq:frame.seq,...received.get(frame.seq),ack:performance.now(),draws:labMedia.draws-draws});
              draws = labMedia.draws; received.delete(frame.seq);
            }
            return send(frame);
          };
          return link;
        };
        labApp.ui.reset('control');
      })()`);
      await pause(1500);
      await evaluate(`(() => {
        const button = [...document.querySelectorAll('[aria-label^="Watch "]')].find(b=>b.getClientRects().length && !b.closest('.is-hidden'));
        if (!button) throw new Error('No target to watch');
        button.click();
      })()`);
      // Polling stays outside the CDP request's 30 s timeout.
      for (let i = 0; i < 70; i++) {
        await pause(500);
        if (await evaluate("labMedia.frames.length > 0")) break;
      }
      const first = await evaluate("labMedia.frames[0] ?? null");
      if (!first) {
        record(
          step,
          await evaluate(
            "({status:'fail',...labMedia,text:document.body.innerText})",
          ),
        );
        screenshot(`${step}-failed`);
        throw new Error("No first frame within 35 seconds");
      }
      await evaluate(
        "labMedia.sampleStart=performance.now(); labMedia.sampleIndex=labMedia.frames.length",
      );
      await pause(step === "watch" ? 31000 : 1000);
      const sample = await evaluate(`(() => {
        const end=performance.now(),frames=labMedia.frames.slice(labMedia.sampleIndex),durationMs=end-labMedia.sampleStart;
        return {...labMedia,sampleEnd:end,durationMs,sampleCount:frames.length,fps:frames.length*1000/durationMs,firstFrameMs:labMedia.frames[0].ack-labMedia.opened,text:document.body.innerText};
      })()`);
      record(step, sample);
      if (
        new Set(sample.frames.map((f) => f.seq)).size !== sample.frames.length
      )
        throw new Error(
          "Duplicate frame acknowledgements invalidate this sample",
        );
      const png = await evaluate(
        "document.querySelector('canvas')?.toDataURL('image/png')",
      );
      if (png)
        fs.writeFileSync(
          path.join(
            evidence,
            step === "watch" ? "phone-canvas.png" : "open-canvas.png",
          ),
          Buffer.from(png.split(",")[1], "base64"),
        );
      screenshot(step);
      console.log(
        JSON.stringify({
          firstFrameMs: sample.firstFrameMs,
          fps: sample.fps,
          sampleCount: sample.sampleCount,
          durationMs: sample.durationMs,
        }),
      );
    } else if (step === "reopen") {
      await click("Reopen sharing");
      let live = false;
      for (let attempt = 0; attempt < 40; attempt++) {
        live = await evaluate(
          "document.body.innerText.includes('\\nLive\\n') && !!document.querySelector('canvas')",
        );
        if (live) break;
        await pause(250);
      }
      record(
        "reopen",
        await evaluate(
          "({targets:labApp.session.state.get().targets,text:document.body.innerText})",
        ),
      );
      screenshot("reopen");
      if (!live) throw Error("Reopened sharing did not deliver a live picture");
    } else if (["input", "hold", "arm", "paste"].includes(step)) {
      let acquire;
      for (let attempt = 0; attempt < 40; attempt++) {
        acquire = await evaluate(
          "[...document.querySelectorAll('.control-footer button')].find(b=>b.getClientRects().length&&!b.closest('.is-hidden')&&!b.disabled&&['Enable control','Take over'].includes(b.textContent.trim()))?.textContent.trim()",
        );
        if (acquire) break;
        await pause(250);
      }
      if (!acquire)
        throw new Error("Current desktop control action unavailable");
      await click(acquire);
      await pause(800);
      const controlled = await evaluate(
        "Object.values(labApp.session.state.get().control).some(v=>v.input_state==='human' && v.lease?.state==='active' && v.lease.owner.kind==='human' && v.lease.owner.device_id===labApp.profile.deviceId)",
      );
      if (!controlled)
        throw Error("The computer did not confirm phone control");
      await evaluate(`(() => {
        window.labOffAck?.(); window.labAcks=[]; window.labOffAck=labApp.session.onInputAck(a=>labAcks.push(a));
        window.labSend = action => {
          const canvas=document.querySelector('canvas');
          let f=canvas?.[Object.keys(canvas).find(k=>k.startsWith('__reactFiber'))];
          for(;f;f=f.return) if(typeof f.memoizedProps?.send==='function') return f.memoizedProps.send(action);
          throw new Error('Desktop UI input callback unavailable');
        };
      })()`);
      const actions =
        step === "arm"
          ? []
          : step === "hold"
            ? [
                { action: "pointer_move", to: { x: 0.5, y: 0.5 } },
                {
                  action: "pointer_button",
                  at: { x: 0.5, y: 0.5 },
                  button: "left",
                  pressed: true,
                },
                { action: "key_press", key: "shift", pressed: true },
              ]
            : [
                { action: "pointer_move", to: { x: 0.5, y: 0.5 } },
                {
                  action: "click",
                  at: { x: 0.5, y: 0.5 },
                  button: "left",
                  count: 1,
                },
                { action: "text", text: "Desktop ASCII 123\n" },
                { action: "text", text: "Café Ελληνικά 日本語\n" },
                { action: "key_chord", keys: ["ctrl", "a"] },
                { action: "key_chord", keys: ["End"] },
                { action: "scroll", at: { x: 0.5, y: 0.5 }, dx: 0, dy: 120 },
              ];
      const sent = [];
      for (const action of actions) {
        sent.push(await evaluate(`labSend(${JSON.stringify(action)})`));
        await pause(500);
      }
      if (step === "paste") {
        record(
          "paste-refusal",
          await evaluate("({acks:labAcks,text:document.body.innerText})"),
        );
        await click("Paste instead");
        await pause(800);
      }
      record(
        step,
        await evaluate(
          `({sent:${JSON.stringify(sent)},acks:labAcks,control:labApp.session.state.get().control,text:document.body.innerText})`,
        ),
      );
      screenshot(step);
      if (step === "input" || step === "paste") await click("Leave control");
    } else if (step === "computer" || step === "computer-human") {
      const projectPath = process.env.LAB_PROJECT_PATH;
      if (!projectPath?.startsWith("/"))
        throw new Error("Set LAB_PROJECT_PATH to the disposable guest project");
      await evaluate(`window.labCommand = async body => {
        const receipt = await labApp.session.command(body, {label:'Desktop qualification'});
        if (receipt.error) throw new Error(receipt.error.message);
        if (receipt.result) return receipt.result;
        for (let i=0;i<60;i++) {
          const status=await labApp.session.query({kind:'command_status',command_id:receipt.command_id});
          if (status.error) throw new Error(status.error.message);
          if (status.stage==='observed') return status.result??status.receipt?.result;
          await new Promise(r=>setTimeout(r,200));
        }
        throw new Error('Command outcome was not observed');
      }`);
      // Start from the Control hub, as the review flow does. Retire any old
      // Desktop view before creating the run: leaving that view deliberately
      // pauses the agent it supervises, and must not race the new run’s grant.
      await evaluate("labApp.ui.reset('control')");
      await pause(750);
      const readiness = await evaluate(
        "({capability:labApp.session.state.get().capabilities?.computer_use,harnesses:labApp.session.state.get().harnesses.map(h=>({kind:h.kind,delegation:h.capabilities.computer_use_delegation}))})",
      );
      record("computer-readiness", readiness);
      if (
        readiness.capability?.state !== "available" ||
        readiness.harnesses.find((h) => h.kind === "claude_code")?.delegation
          .state !== "available"
      )
        throw new Error("Computer use is unavailable");
      const project = await evaluate(
        `Object.values(labApp.session.state.get().projects).find(p=>p.host_path===${JSON.stringify(projectPath)}) ?? labCommand({command:'add_project',host_path:${JSON.stringify(projectPath)},name:'Desktop qualification'})`,
      );
      // A title no other task shares, so its Stop cannot be confused with another's.
      const title = `Scripted Stop fence ${crypto.randomUUID().slice(0, 8)}`;
      const task = await evaluate(
        `labCommand({command:'create_task',project_id:${JSON.stringify(project.project_id)},kind:'computer_use',title:${JSON.stringify(title)},prompt:${JSON.stringify(step === "computer-human" ? "Request the private fixture step." : "Exercise the disposable input witness.")},bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:labApp.session.state.get().targets[0].target_id,placement:{mode:'start_now'}})`,
      );
      // The normal task view keeps supervision active while the specialist works.
      await evaluate(
        `labApp.ui.push({screen:'computer_use',taskId:${JSON.stringify(task.task_id)}},'control')`,
      );
      await pause(5000);
      record(
        step === "computer-human"
          ? "computer-before-private"
          : "computer-before-stop",
        await evaluate(
          "({snapshot:labApp.session.state.get(),text:document.body.innerText})",
        ),
      );
      screenshot(
        step === "computer-human"
          ? "computer-before-private"
          : "computer-before-stop",
      );
      if (step === "computer-human") {
        await click("Start private step");
        for (let attempt = 0; attempt < 80; attempt++) {
          if (await evaluate("Object.values(labApp.session.state.get().control).some(v=>v.private_step?.ready)")) break;
          await pause(250);
        }
        const result = await evaluate(
          "({snapshot:labApp.session.state.get(),text:document.body.innerText})",
        );
        record("computer-private", result);
        screenshot("computer-private");
        if (
          !Object.values(result.snapshot.control).some(
            (view) => view.private_step?.ready,
          )
        )
          throw Error("Private step was not confirmed ready");
      } else {
        await evaluate(`(() => {
          window.labStopTiming={tap:null,revoked:null,receipt:null,activeBeforeTap:false,updates:[]};
          const taskId=${JSON.stringify(task.task_id)};
          const targetId=labApp.session.state.get().tasks[taskId].target_id;
          window.labStopOff?.();
          window.labStopOff=labApp.session.state.subscribe(()=>{
            const s=labApp.session.state.get(),v=s.control[targetId],at=performance.now();
            if(labStopTiming.tap===null) return;
            if(v?.lease?.state==='revoked' && v.model_observation?.state==='withdrawn' && labStopTiming.revoked===null) labStopTiming.revoked=at;
            const receipt=s.interventions.find(r=>r.taskId===taskId && r.receipt.kind==='stop_task');
            if(receipt && labStopTiming.receipt===null) labStopTiming.receipt={...receipt,observedAt:at};
            labStopTiming.updates.push({at,input:v?.input_state,lease:v?.lease?.state,observation:v?.model_observation?.state});
          });
          const isTaskStop=${isTaskStop.toString()};
          const title=${JSON.stringify(title)};
          const button=[...document.querySelectorAll('button')].find(b=>b.getClientRects().length && !b.closest('.is-hidden') && isTaskStop(b.getAttribute('aria-label'),b.querySelector('.named-stop__display')?.textContent,title));
          if(!button || button.disabled) throw Error('Stop for this task is unavailable');
          const before=labApp.session.state.get().control[targetId];
          labStopTiming.activeBeforeTap=before?.input_state==='agent' && before.lease?.state==='active' && before.model_observation?.state==='allowed';
          labStopTiming.tap=performance.now();
          button.click();
        })()`);
        await pause(3500);
        const result = await evaluate(
          `({taskId:${JSON.stringify(task.task_id)},timing:labStopTiming,snapshot:labApp.session.state.get(),text:document.body.innerText})`,
        );
        record("computer-stop", result);
        screenshot("computer-stop");
        await evaluate("labStopOff?.()");
        if (!result.timing.activeBeforeTap)
          throw Error("Agent input was already off before Stop; repeat the mid-action check");
        if (result.timing.revoked === null || result.timing.receipt === null)
          throw Error("Stop withdrawal or receipt unconfirmed");
        console.log(
          JSON.stringify({
            stopTapToRevocationObservedMs:
              result.timing.revoked - result.timing.tap,
            facts: result.timing.receipt.receipt.facts,
          }),
        );
      }
    } else if (step === "task") {
      // One authored-harness task in the project the computer's owner added:
      // the fixture asks through the archive's MCP bridge, the phone allows it,
      // and the run is followed to its receipt.
      const projectPath = process.env.LAB_PROJECT_PATH;
      if (!projectPath?.startsWith("/"))
        throw new Error("Set LAB_PROJECT_PATH to the project added on the guest");
      for (let n = 0; ; n++) {
        if (await evaluate("labApp.session.link.get().state==='online' && labApp.session.state.get().caughtUp")) break;
        if (n === 120) throw new Error("The phone is not connected to the computer");
        await pause(250);
      }
      const started = await evaluate(`(async () => {
        const s = labApp.session;
        const project = Object.values(s.state.get().projects).find(p => p.host_path === ${JSON.stringify(projectPath)});
        if (!project) throw new Error('The project added at the computer is not listed');
        const r = await s.command({command:'create_task',project_id:project.project_id,kind:'coding',mode:'work',title:'Release qualification',prompt:'bridge-permission',bundle:null,harness:'claude_code',model:null,effort:null,mission:null,target_id:null,placement:{mode:'start_now'}},{label:'Release qualification task'});
        if (r.result?.kind !== 'task_created') throw new Error('Task not created: ' + JSON.stringify(r.error ?? r.result));
        labApp.ui.selectTask(project.project_id, r.result.task_id); labApp.ui.reset('talk');
        return {projectId: project.project_id, taskId: r.result.task_id};
      })()`);
      const pending = `Object.values(labApp.session.state.get().approvals).find(a=>a.task_id===${JSON.stringify(started.taskId)}&&a.state==='pending')`;
      for (let n = 0; !(await evaluate(`Boolean(${pending})`)); n++) {
        if (n === 120) throw new Error("The agent did not ask for approval");
        await pause(250);
      }
      screenshot("task-approval");
      await evaluate(`(async () => {
        const a = ${pending};
        const r = await labApp.session.command({command:'resolve_approval',approval_id:a.approval_id,decision:{decision:'allow'}},{label:'Allow',expectedRevision:a.revision});
        if (r.error) throw new Error(r.error.message);
      })()`);
      const finished = `['completed','failed','interrupted'].includes(labApp.session.state.get().tasks[${JSON.stringify(started.taskId)}]?.state)`;
      for (let n = 0; !(await evaluate(finished)); n++) {
        if (n === 240) throw new Error("The task did not finish within a minute");
        await pause(250);
      }
      await pause(800);
      screenshot("task-conversation");
      await click("What happened");
      for (let n = 0; !(await evaluate("location.hash.includes('receipt') || document.querySelector('[aria-label^=\"Choose run:\"]') !== null")); n++) {
        if (n === 40) break;
        await pause(250);
      }
      await pause(800);
      const result = await evaluate(`(() => {
        const s = labApp.session.state.get(), task = s.tasks[${JSON.stringify(started.taskId)}];
        return {task, run: s.runs[task.current_run], route: labApp.ui.route(), text: document.body.innerText};
      })()`);
      record("task", { ...started, ...result });
      screenshot("task-receipt");
      if (result.task.state !== "completed" || result.route.screen !== "receipt")
        throw new Error("The task did not complete to a receipt");
      console.log(JSON.stringify({ task: result.task.state, run: result.run?.state }));
    } else if (step === "revoked") {
      // After the computer's owner revoked this phone: it must stay refused.
      for (let n = 0; n < 40; n++) {
        if (await evaluate("labApp.session.link.get().state==='failed'")) break;
        await pause(250);
      }
      await evaluate("labApp.session.reconnect()");
      await pause(2000);
      await evaluate("labApp.ui.reset('work',[{screen:'connections'}])");
      await pause(800);
      const result = await evaluate(
        "({link:labApp.session.link.get(),online:labApp.session.online,text:document.body.innerText})",
      );
      record("revoked", result);
      screenshot("revoked");
      if (result.online || result.link.state !== "failed" || result.link.reason !== "revoked")
        throw new Error("The revoked phone is not shown as revoked");
    } else if (step === "snapshot") {
      record(
        "snapshot",
        await evaluate(
          "({snapshot:labApp.session.state.get(),link:labApp.session.link.get(),text:document.body.innerText})",
        ),
      );
      screenshot("snapshot");
    } else {
      throw new Error("Unknown phone qualification step");
    }
  }
} finally {
  c.close();
}
