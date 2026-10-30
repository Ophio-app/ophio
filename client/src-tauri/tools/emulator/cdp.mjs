// SPDX-License-Identifier: Apache-2.0
// Connect only to the forwarded, isolated emulator WebView. No desktop browser.
export const devtoolsPort = Number(process.env.OPHIO_DEVTOOLS_PORT ?? 9226);
export async function connect(port = devtoolsPort) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find(t => t.type === 'page' && t.url.startsWith('http://tauri.localhost'));
  if (!target) throw new Error('The packaged app WebView is unavailable.');
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let id = 0;
  const pending = new Map();
  socket.onmessage = ({data}) => {
    const message = JSON.parse(data); const p = pending.get(message.id);
    if (p) { pending.delete(message.id); clearTimeout(p.timer); if (message.error) p.reject(new Error(message.error.message)); else p.resolve(message.result); }
  };
  const send = (method, params) => new Promise((resolve, reject) => {
    const n = ++id;
    const timer = setTimeout(() => { pending.delete(n); reject(new Error(`CDP ${method} timed out`)); }, 30000);
    pending.set(n, {resolve, reject, timer}); socket.send(JSON.stringify({id:n, method, params}));
  });
  return {
    send,
    async evaluate(expression) {
      const result = await send('Runtime.evaluate', {expression, awaitPromise:true, returnByValue:true, userGesture:true});
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
      return result.result.value;
    },
    close() { socket.close(); for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error('CDP closed')); } pending.clear(); },
  };
}
export const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
