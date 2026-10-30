#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// Phone tools for reviewers of the connected review environment: the whole
// Android screen, real touches, device settings and failure drills, none of
// which the WebView's DevTools can reach. A stdio MCP server with no
// dependencies; see README.md.
import { execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const serial = "emulator-5562";
const devtools = "http://127.0.0.1:9226";
const connected = fileURLToPath(new URL("connected.sh", import.meta.url));
const versions = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const run = promisify(execFile);

if (process.env.ADB_SERVER_SOCKET !== "tcp:5038") {
  console.error(
    "Start this server with the isolated ADB server (ADB_SERVER_SOCKET=tcp:5038).",
  );
  process.exit(1);
}

const adb = async (...args) =>
  (
    await run("adb", ["-s", serial, ...args], {
      encoding: "buffer",
      maxBuffer: 64 << 20,
    })
  ).stdout;
const shell = async (...args) => (await adb("shell", ...args)).toString().trim();
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const text = (value) => [{ type: "text", text: value }];

async function until(condition, what) {
  for (let n = 0; n < 20; n++) {
    if (await condition()) return;
    await pause(150);
  }
  throw new Error(`Timed out waiting for ${what}.`);
}

const keyboardShown = async () =>
  (await shell("dumpsys", "input_method")).includes("mInputShown=true");

// Evaluate an expression in the app's page over its forwarded DevTools socket.
async function evaluate(expression) {
  const pages = await (await fetch(`${devtools}/json/list`)).json();
  const page = pages.find(
    (p) => p.type === "page" && p.url.startsWith("http://tauri.localhost"),
  );
  if (!page) throw new Error("The app's WebView is not forwarded; run connected.sh up.");
  const socket = new WebSocket(page.webSocketDebuggerUrl);
  try {
    await new Promise((resolve, reject) => {
      socket.onopen = resolve;
      socket.onerror = () => reject(new Error("The app's WebView refused DevTools."));
    });
    const reply = await new Promise((resolve) => {
      socket.onmessage = ({ data }) => resolve(JSON.parse(data));
      socket.send(
        JSON.stringify({
          id: 1,
          method: "Runtime.evaluate",
          params: { expression, returnByValue: true },
        }),
      );
    });
    return reply.result.result.value;
  } finally {
    socket.close();
  }
}

// The centre of the focused text field, in device pixels.
async function focusedField() {
  const field = await evaluate(`(() => {
    const e = document.activeElement;
    if (!e?.matches("input, textarea, [contenteditable]:not([contenteditable=false])")) return null;
    const r = e.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2, scale: devicePixelRatio };
  })()`);
  if (!field) return null;
  const screen = (await adb("exec-out", "uiautomator", "dump", "/dev/tty")).toString();
  const bounds = screen.match(
    /class="android\.webkit\.WebView" package="app\.ophio"[^>]*bounds="\[(\d+),(\d+)\]/,
  );
  if (!bounds) throw new Error("The app's WebView is not on screen.");
  return {
    x: Math.round(Number(bounds[1]) + field.x * field.scale),
    y: Math.round(Number(bounds[2]) + field.y * field.scale),
  };
}

const point = (description) => ({
  type: "integer",
  minimum: 0,
  description,
});

const tools = {
  screenshot: {
    description:
      "Capture the whole Android screen, including the system bars, the on-screen keyboard and Android dialogs. Tap and swipe take the device pixels reported with it. Optionally also save the PNG to a file.",
    inputSchema: {
      type: "object",
      properties: {
        file: {
          type: "string",
          description: "Absolute path to also save the PNG to",
        },
      },
      additionalProperties: false,
    },
    async run({ file }) {
      const png = await adb("exec-out", "screencap", "-p");
      if (file) {
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, png);
      }
      const size = `${png.readUInt32BE(16)}×${png.readUInt32BE(20)}`;
      return [
        { type: "image", data: png.toString("base64"), mimeType: "image/png" },
        ...text(`Screen ${size} device pixels${file ? `, saved to ${file}` : ""}.`),
      ];
    },
  },
  tap: {
    description: "Tap the screen at device pixels, as a finger would.",
    inputSchema: {
      type: "object",
      properties: { x: point("Device pixels from the left"), y: point("Device pixels from the top") },
      required: ["x", "y"],
      additionalProperties: false,
    },
    async run({ x, y }) {
      await shell("input", "tap", String(x), String(y));
      return text(`Tapped ${x}, ${y}.`);
    },
  },
  swipe: {
    description: "Swipe between two points in device pixels, for scrolling and edge gestures.",
    inputSchema: {
      type: "object",
      properties: {
        from_x: point("Start, device pixels from the left"),
        from_y: point("Start, device pixels from the top"),
        to_x: point("End, device pixels from the left"),
        to_y: point("End, device pixels from the top"),
        duration_ms: { type: "integer", minimum: 50, maximum: 5000, default: 300 },
      },
      required: ["from_x", "from_y", "to_x", "to_y"],
      additionalProperties: false,
    },
    async run({ from_x, from_y, to_x, to_y, duration_ms = 300 }) {
      await shell("input", "swipe", ...[from_x, from_y, to_x, to_y, duration_ms].map(String));
      return text(`Swiped from ${from_x}, ${from_y} to ${to_x}, ${to_y}.`);
    },
  },
  back: {
    description: "Press Android Back. With the keyboard up, this closes the keyboard instead.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
    async run() {
      await shell("input", "keyevent", "KEYCODE_BACK");
      return text("Pressed Back.");
    },
  },
  keyboard: {
    description:
      "Show or hide the on-screen keyboard. Showing it taps the app's focused text field, so focus one first (for example by clicking it in the app).",
    inputSchema: {
      type: "object",
      properties: { visible: { type: "boolean" } },
      required: ["visible"],
      additionalProperties: false,
    },
    async run({ visible }) {
      if ((await keyboardShown()) === visible)
        return text(`The keyboard is already ${visible ? "showing" : "hidden"}.`);
      if (visible) {
        const field = await focusedField();
        if (!field) throw new Error("No text field in the app has focus.");
        await shell("input", "tap", String(field.x), String(field.y));
      } else {
        await shell("input", "keyevent", "KEYCODE_BACK");
      }
      await until(async () => (await keyboardShown()) === visible, "the keyboard");
      return text(`The keyboard is ${visible ? "showing" : "hidden"}.`);
    },
  },
  rotate: {
    description: "Turn the phone to portrait or landscape and hold it there.",
    inputSchema: {
      type: "object",
      properties: { orientation: { enum: ["portrait", "landscape"] } },
      required: ["orientation"],
      additionalProperties: false,
    },
    async run({ orientation }) {
      const [rotation, degrees] = orientation === "portrait" ? ["0", "0"] : ["1", "90"];
      await shell("settings", "put", "system", "accelerometer_rotation", "0");
      await shell("settings", "put", "system", "user_rotation", rotation);
      await until(
        async () =>
          (await shell("dumpsys", "window", "displays")).includes(
            `mCurrentRotation=ROTATION_${degrees}`,
          ),
        "the screen to turn",
      );
      return text(`Rotated to ${orientation}.`);
    },
  },
  font_scale: {
    description: "Set Android's font size, as in Settings › Display › Font size.",
    inputSchema: {
      type: "object",
      properties: { scale: { enum: [1.0, 1.3, 2.0] } },
      required: ["scale"],
      additionalProperties: false,
    },
    async run({ scale }) {
      await shell("settings", "put", "system", "font_scale", scale.toFixed(1));
      return text(`Font scale is ${scale.toFixed(1)}.`);
    },
  },
  dark_theme: {
    description: "Turn Android's dark theme on or off.",
    inputSchema: {
      type: "object",
      properties: { on: { type: "boolean" } },
      required: ["on"],
      additionalProperties: false,
    },
    async run({ on }) {
      await shell("cmd", "uimode", "night", on ? "yes" : "no");
      return text(`Dark theme is ${on ? "on" : "off"}.`);
    },
  },
  companion: {
    description:
      "Failure drill on this environment's companion, by its exact PID: pause freezes it (the phone's connection hangs), resume thaws it, restart stops it and starts it again with the same state.",
    inputSchema: {
      type: "object",
      properties: { action: { enum: ["pause", "resume", "restart"] } },
      required: ["action"],
      additionalProperties: false,
    },
    async run({ action }) {
      const { stdout } = await run("bash", [connected, "companion", action]);
      return text(stdout.trim());
    },
  },
  network: {
    description:
      "Failure drill: cut the emulator off every network with airplane mode, or restore it.",
    inputSchema: {
      type: "object",
      properties: { connected: { type: "boolean" } },
      required: ["connected"],
      additionalProperties: false,
    },
    async run({ connected }) {
      await shell("cmd", "connectivity", "airplane-mode", connected ? "disable" : "enable");
      return text(connected ? "The network is restored." : "The network is cut.");
    },
  },
};

const send = (message) =>
  process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);

async function answer({ method, params }) {
  switch (method) {
    case "initialize":
      return {
        protocolVersion: versions.includes(params?.protocolVersion)
          ? params.protocolVersion
          : versions[0],
        capabilities: { tools: {} },
        serverInfo: { name: "phone", version: "1.0.0" },
      };
    case "ping":
      return {};
    case "tools/list":
      return {
        tools: Object.entries(tools).map(([name, { description, inputSchema }]) => ({
          name,
          description,
          inputSchema,
        })),
      };
    case "tools/call": {
      const tool = tools[params?.name];
      if (!tool) throw Object.assign(new Error(`Unknown tool ${params?.name}`), { code: -32602 });
      try {
        return { content: await tool.run(params.arguments ?? {}) };
      } catch (error) {
        const detail = error.stderr?.toString().trim() || error.message;
        return { content: text(detail), isError: true };
      }
    }
    default:
      throw Object.assign(new Error(`Unknown method ${method}`), { code: -32601 });
  }
}

// Device actions run one at a time, in the order they were asked for.
let queue = Promise.resolve();
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return send({ id: null, error: { code: -32700, message: "Parse error" } });
  }
  if (request.id === undefined) return;
  queue = queue.then(async () => {
    try {
      send({ id: request.id, result: await answer(request) });
    } catch (error) {
      send({ id: request.id, error: { code: error.code ?? -32603, message: error.message } });
    }
  });
});
