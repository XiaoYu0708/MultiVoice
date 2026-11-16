// Headless Chrome test runner for the web client (no extension needed).
//
//   node tools/headless.mjs <url> [seconds] [--shot out.png] [--eval "js expr"] [--mobile] [--size WxH] [--no-mic]
//
// - Prints the page's console output and uncaught exceptions to stdout
// - Fake microphone (Chrome's built-in beep) and auto-granted permissions, autoplay allowed
// - --eval runs the expression after <seconds> and prints the result (JSON); --eval sleep:N waits N seconds
// - --shot saves a screenshot after <seconds>
// - --no-mic: getUserMedia fails with NotFoundError (a computer without a microphone)
// Uses a throwaway profile directory; Chrome is killed at the end.
import { spawn } from 'node:child_process';
import { mkdtempSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const args = process.argv.slice(2);
const url = args[0];
const seconds = Number(args[1] ?? 10);
const opt = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
const shot = opt('--shot');
const evals = args.flatMap((a, i) => (a === '--eval' ? [args[i + 1]] : []));
const mobile = args.includes('--mobile');
const noMic = args.includes('--no-mic');
const size = opt('--size')?.split('x').map(Number); // e.g. --size 512x512 (CSS px, scale 1)

const candidates = [
  process.env.CHROME,
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
].filter(Boolean);
const chrome = candidates.find((p) => existsSync(p));
if (!url || !chrome) {
  console.error('usage: node tools/headless.mjs <url> [seconds] [--shot out.png] [--eval expr] [--mobile] [--no-mic]');
  process.exit(2);
}

const profile = mkdtempSync(join(tmpdir(), 'mv-headless-'));
const port = 9300 + Math.floor(Math.random() * 500);
const proc = spawn(chrome, [
  '--headless=new',
  `--remote-debugging-port=${port}`,
  `--user-data-dir=${profile}`,
  '--no-first-run',
  '--use-fake-ui-for-media-stream',
  '--use-fake-device-for-media-stream',
  '--autoplay-policy=no-user-gesture-required',
  mobile ? '--window-size=390,844' : '--window-size=1200,900',
  'about:blank',
], { stdio: 'ignore' });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 50 && !target; i++) {
  await sleep(200);
  try {
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    target = list.find((t) => t.type === 'page');
  } catch {}
}
if (!target) { console.error('chrome did not start'); proc.kill(); process.exit(1); }

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r, { once: true }));
let id = 0;
const pending = new Map();
const t0 = Date.now();
const stamp = () => ((Date.now() - t0) / 1000).toFixed(1).padStart(5);
ws.addEventListener('message', (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); return; }
  if (msg.method === 'Runtime.consoleAPICalled') {
    const text = msg.params.args.map((a) => a.value ?? a.description ?? a.type).join(' ');
    console.log(`[${stamp()}] ${msg.params.type}: ${text}`);
  } else if (msg.method === 'Runtime.exceptionThrown') {
    const d = msg.params.exceptionDetails;
    console.log(`[${stamp()}] EXCEPTION: ${d.exception?.description ?? d.text}`);
  } else if (msg.method === 'Log.entryAdded') {
    console.log(`[${stamp()}] log/${msg.params.entry.level}: ${msg.params.entry.text}`);
  }
});
const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });

await send('Runtime.enable');
await send('Log.enable');
if (mobile) await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
if (size) await send('Emulation.setDeviceMetricsOverride', { width: size[0], height: size[1], deviceScaleFactor: 1, mobile: false });
if (noMic) {
  await send('Page.enable');
  await send('Page.addScriptToEvaluateOnNewDocument', {
    source: "navigator.mediaDevices.getUserMedia = () => Promise.reject(new DOMException('Requested device not found', 'NotFoundError'));",
  });
}
await send('Page.navigate', { url });
await sleep(seconds * 1000);
for (const e of evals) {
  if (e.startsWith('sleep:')) { await sleep(Number(e.slice(6)) * 1000); continue; }
  const r = await send('Runtime.evaluate', { expression: e, awaitPromise: true, returnByValue: true });
  console.log(`[${stamp()}] eval ${e} =>`, JSON.stringify(r.result?.result?.value ?? r.result?.exceptionDetails?.exception?.description ?? r.result));
}
if (shot) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(shot, Buffer.from(r.result.data, 'base64'));
  console.log(`[${stamp()}] screenshot -> ${shot}`);
}
ws.close();
proc.kill();
await sleep(500);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
process.exit(0);
