// MultiVoice web client: UI + glue between the network (wasm-bindgen module `web`)
// and the audio engine running in an AudioWorklet (`worklet.js` + `web_audio.wasm`).
import init, * as mv from './pkg/web.js';

const $ = (s) => document.querySelector(s);
const store = {
  get(k, d) { try { const v = localStorage.getItem('mv.' + k); return v === null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem('mv.' + k, JSON.stringify(v)); } catch {} },
};
const MODE_LABEL = { vad: '偵測說話', ptt: '按住說話', off: '不說話' };
const split = (m) => [Number(m & 0xffffffffn), Number(m >> 32n)];
const join64 = (lo, hi) => (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0);

// ---------------------------------------------------------------- state
let room = null;
let ctx = null, node = null, micStream = null, micSource = null, wakeLock = null;
let state = null;              // last Room.state() JSON
let audioStatus = null;        // last status from the worklet
let muted = false, deafened = false;
let leaving = false;
const settings = {
  volume: store.get('volume', 1),
  gate: store.get('gate', -50),
  denoise: store.get('denoise', true),
  cues: store.get('cues', true),
  cueVolume: store.get('cueVolume', 1),
  wakelock: store.get('wakelock', true),
};
/** channel id -> { mode, listen, volume } for channels I joined */
const joined = new Map();
/** channel ids whose PTT button is held */
const held = new Set();
const cards = new Map();
/** how loud I hear each member: display name -> gain 0..10 (long-press / right-click a name). By name because
 * web members get a new identity every time */
const memberVolumes = store.get('memberVolumes', {});
/** member idx -> gain last sent to the worklet */
let sentGains = new Map();
/** channel ids whose member list is expanded (collapsed by default) */
const expanded = new Set();
let sentSlots = new Map();     // slot -> JSON of what was sent to the worklet

const chanPrefs = () => store.get('channels', {});
/** `withMode = false`：只存音量，保留原本存的說話方式（例如被房主移進、暫時只收聽時調音量） */
function savePrefs(name, cfg, withMode = true) {
  const all = chanPrefs();
  all[name] = { mode: withMode ? cfg.mode : all[name]?.mode, volume: cfg.volume };
  store.set('channels', all);
}

// ---------------------------------------------------------------- join screen
await init();
$('#version').textContent = ' ' + mv.version();
$('#name').value = store.get('name', '');
if (location.hash.length > 1) $('#ticket').value = decodeURIComponent(location.hash.slice(1));

$('#join-form').addEventListener('submit', async (e) => {
  e.preventDefault();
  const name = $('#name').value.trim();
  const ticket = $('#ticket').value.trim().replace(/^.*#/, '');
  const err = mv.check_ticket(ticket);
  if (!name) return showJoinError('請輸入名稱');
  if (err) return showJoinError('邀請連結格式錯誤：' + err);
  store.set('name', name);
  $('#join-btn').disabled = true;
  $('#join-btn').textContent = '連線中…';
  showJoinError('');
  try {
    await startAudio();          // must run inside the click (iOS)
    room = await mv.join(ticket, name);
    history.replaceState(null, '', '#' + ticket);
    enterRoom();
  } catch (ex) {
    stopAudio();
    showJoinError(String(ex.message || ex));
  } finally {
    $('#join-btn').disabled = false;
    $('#join-btn').textContent = '加入房間';
  }
});

function showJoinError(msg) {
  $('#join-error').textContent = msg;
  $('#join-error').hidden = !msg;
}

// ---------------------------------------------------------------- audio
// ?ios in the URL: test the iPhone code paths on another browser
const IOS = /iPhone|iPad|iPod/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
  || new URLSearchParams(location.search).has('ios');
const MOBILE = IOS || /Android/i.test(navigator.userAgent);
/** the user granted the mic once: try to bring it back automatically after the OS paused it */
let micWanted = false;
/** why there is no mic: '' | 'none' (no device) | 'denied' (no permission) | 'unsupported' (browser / not https) */
let micProblem = '';

async function startAudio() {
  // every join starts with the mic off (user preference, 2026-11-16); tap 「麥克風已關」 to talk
  muted = true;
  deafened = false;
  renderToggles();
  // Safari 16.4+: tell iOS this is a call (mic + playback together)
  setAudioSession('play-and-record');
  ctx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
  await ctx.resume();
  // iOS puts the context into 'interrupted'/'suspended' in the background or during a phone call
  ctx.addEventListener('statechange', () => {
    if (ctx && ctx.state !== 'running' && document.visibilityState === 'visible') ctx.resume().catch(() => {});
    renderAudio();
  });
  if (ctx.sampleRate !== 48000) throw new Error(`這個瀏覽器不支援 48 kHz 音訊（${ctx.sampleRate} Hz）`);
  const [bytes] = await Promise.all([
    fetch('web_audio.wasm').then((r) => { if (!r.ok) throw new Error('無法載入音訊引擎'); return r.arrayBuffer(); }),
    ctx.audioWorklet.addModule('worklet.js'),
  ]);
  node = new AudioWorkletNode(ctx, 'mv-engine', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [2] });
  node.port.onmessage = (e) => onWorklet(e.data);
  node.port.postMessage({ t: 'wasm', bytes }, [bytes]);
  node.connect(ctx.destination);
  if ('mediaSession' in navigator) {
    try { navigator.mediaSession.metadata = new MediaMetadata({ title: 'MultiVoice', artist: '語音房間' }); } catch {}
  }
  sentSlots = new Map();
  sentGains = new Map();
  pushSettings();
  await enableMic();
}

/** Open the microphone (also used to re-open it after the OS cut it off in the background) */
async function enableMic() {
  micStream?.getTracks().forEach((t) => t.stop());
  micStream = null;
  try { micSource?.disconnect(); } catch {}
  micSource = null;
  if (!navigator.mediaDevices?.getUserMedia) {
    micProblem = 'unsupported';
    showMicWarning('這個瀏覽器不能使用麥克風，只能聽。', false);
    return;
  }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: micConstraints() });
    if (!ctx || !node) { stream.getTracks().forEach((t) => t.stop()); return; }
    micStream = stream;
    micWanted = true;
    micProblem = '';
    // phones: keep talking after switching to a game or locking the screen (see startRtcKeepAlive)
    if (MOBILE) startRtcKeepAlive(stream.getAudioTracks()[0]);
    micSource = ctx.createMediaStreamSource(stream);
    micSource.connect(node);
    const track = stream.getAudioTracks()[0];
    // phones stop the mic when the page goes to the background
    track?.addEventListener('ended', () => {
      if (micStream !== stream) return;
      micStream = null;
      if (document.visibilityState === 'visible') showMicWarning('麥克風被系統中斷了（例如切到其他 App）。', true);
    });
    // iOS: the track is only paused ("muted") while in the background or during a call
    track?.addEventListener('mute', () => { if (micStream === stream) renderAudio(); });
    track?.addEventListener('unmute', () => { if (micStream === stream) renderAudio(); });
    showMicWarning('', false);
  } catch (e) {
    // no microphone at all: listen only, and open it automatically when one is plugged in (devicechange)
    if (e?.name === 'NotFoundError' || e?.name === 'OverconstrainedError' || e?.name === 'NotReadableError') {
      micProblem = 'none';
      showMicWarning('沒有偵測到麥克風，只能聽。插上麥克風後會自動開啟。', true);
    } else {
      micProblem = 'denied';
      showMicWarning('沒有麥克風權限，只能聽。', true);
    }
  }
  renderAudio();
}
// a microphone was plugged in while listening without one
navigator.mediaDevices?.addEventListener?.('devicechange', () => {
  if (ctx && node && !micStream && micProblem === 'none') enableMic();
});

// 強力降噪（DeepFilterNet3）已拿掉：會讓聲音變悶、較慢的裝置會斷音。舊的 'strong' 設定直接忽略。
// web_dfn.wasm 與 worklet.js 的載入程式先留著，只是不再用到。

/** Our own RNNoise (same as the desktop app) replaces the browser's noise suppression; denoising twice
 *  makes voices sound muffled. Echo cancellation always stays on (phones use the speaker). */
function micConstraints() {
  return { echoCancellation: true, noiseSuppression: !settings.denoise, autoGainControl: true, channelCount: 1 };
}
function applyDenoise() {
  pushSettings();
  micStream?.getAudioTracks()[0]?.applyConstraints(micConstraints()).catch(() => {});
}

function showMicWarning(text, retry) {
  $('#mic-warning-text').textContent = text;
  $('#mic-retry').hidden = !retry;
  $('#mic-warning').hidden = !text;
}
$('#mic-retry').addEventListener('click', () => { ctx?.resume(); enableMic(); });

function stopAudio() {
  stopRtcKeepAlive();
  micStream?.getTracks().forEach((t) => t.stop());
  micStream = null;
  micWanted = false;
  micProblem = '';
  try { node?.disconnect(); } catch {}
  node = null;
  ctx?.close().catch(() => {});
  ctx = null;
}

function post(m, transfer) { node?.port.postMessage(m, transfer); }

function pushSettings() {
  post({ t: 'volume', v: settings.volume });
  post({ t: 'cueVolume', v: settings.cueVolume });
  post({ t: 'gate', db: settings.gate });
  post({ t: 'denoise', on: settings.denoise });
  post({ t: 'flags', muted, deafened, cues: settings.cues });
}

function onWorklet(m) {
  if (m.t === 'out') room?.send_packets(new Uint8Array(m.b));
  else if (m.t === 'status') { audioStatus = parseStatus(m.s); renderAudio(); }
  else if (m.t === 'error') log('音訊引擎錯誤：' + m.msg);
}

function parseStatus(s) {
  const st = {
    tx: join64(s[1], s[2]), active: !!s[3], voiced: !!s[4], level: s[5] / 10,
    decoded: s[6], fec: s[7], plc: s[8], late: s[9], speaking: new Map(),
  };
  for (let i = 0; i < s[10]; i++) st.speaking.set(s[11 + i * 3], join64(s[12 + i * 3], s[13 + i * 3]));
  return st;
}

/** Recompute slot table + talk masks from `joined` and the slots the host assigned */
function syncAudio() {
  if (!state) return;
  let open = 0n, vad = 0n, ptt = 0n;
  const want = new Map();
  for (const ch of state.channels) {
    const cfg = joined.get(ch.id);
    if (!cfg || ch.slot == null) continue;
    const bit = 1n << BigInt(ch.slot);
    want.set(ch.slot, { volume: cfg.listen ? cfg.volume : 0, pan: 0, priority: false });
    if (cfg.mode === 'open') open |= bit;
    if (cfg.mode === 'vad') vad |= bit;
    if (cfg.mode === 'ptt' && held.has(ch.id)) ptt |= bit;
  }
  for (const [slot, mix] of want) {
    const key = JSON.stringify(mix);
    if (sentSlots.get(slot) !== key) { post({ t: 'slot', slot, active: true, ...mix }); sentSlots.set(slot, key); }
  }
  for (const slot of [...sentSlots.keys()]) {
    if (!want.has(slot)) { post({ t: 'slot', slot, active: false, volume: 0, pan: 0, priority: false }); sentSlots.delete(slot); }
  }
  post({ t: 'talk', open: split(open), vad: split(vad) });
  post({ t: 'ptt', m: split(ptt) });
}

// ---------------------------------------------------------------- room
function enterRoom() {
  leaving = false;
  $('#join-view').hidden = true;
  $('#room-view').hidden = false;
  $('#log').replaceChildren();
  log('已加入房間');
  refresh();
  // auto-join the lobby (channel 0), like the desktop app
  if (state?.channels.some((c) => c.id === 0)) joinChannel(0);
  eventLoop(room);
  voiceLoop(room);
  setWakeLock(settings.wakelock);
  $('#mobile-note').hidden = !MOBILE;
}

// ---------------------------------------------------------------- keep talking in the background (phones)
// iOS stops recording from a page in the background, unless the page is in a WebRTC call (that is how Discord's web
// client keeps working). Our audio does not use WebRTC, so we build a call that never leaves the phone (two peer
// connections in this page) and attach the mic track to it; iOS then treats the page as a call and keeps the mic
// and playback running. Audio itself still goes through our own engine and connection.
// Verified on iPhone 2026-11-16: switching to a game or locking the screen keeps both talking and listening.
let rtc = null;

function rtcActive() {
  return !!rtc && ['connected', 'connecting', 'new'].includes(rtc.a.connectionState);
}

function stopRtcKeepAlive() {
  if (!rtc) return;
  try { rtc.a.close(); rtc.b.close(); } catch {}
  rtc = null;
}

async function startRtcKeepAlive(track) {
  stopRtcKeepAlive();
  if (!track || typeof RTCPeerConnection === 'undefined') return;
  const a = new RTCPeerConnection(), b = new RTCPeerConnection();
  const me = { a, b };
  rtc = me;
  try {
    a.onicecandidate = (e) => { if (e.candidate) b.addIceCandidate(e.candidate).catch(() => {}); };
    b.onicecandidate = (e) => { if (e.candidate) a.addIceCandidate(e.candidate).catch(() => {}); };
    a.addTrack(track, new MediaStream([track]));
    a.onconnectionstatechange = () => {
      if (rtc !== me) return;
      if (a.connectionState === 'failed') log('背景通話連線失敗：切到其他 App 時可能會斷麥克風');
    };
    const offer = await a.createOffer();
    await a.setLocalDescription(offer);
    await b.setRemoteDescription(offer);
    const answer = await b.createAnswer();
    await b.setLocalDescription(answer);
    await a.setRemoteDescription(answer);
  } catch (ex) {
    log('無法建立背景通話連線：' + (ex.message || ex));
    if (rtc === me) stopRtcKeepAlive();
  }
}

async function eventLoop(r) {
  for (;;) {
    const e = await r.next_event();
    if (r !== room) return;
    if (e === undefined) { exitRoom('連線已結束'); return; }
    const ev = JSON.parse(e);
    if (ev.type === 'disconnected') { exitRoom(ev.text); return; }
    if (ev.type === 'invite') { history.replaceState(null, '', '#' + r.ticket()); log(ev.text); }
    if (ev.type === 'log' || ev.type === 'error') log(ev.text);
    if (ev.type === 'moved') onMoved(ev);
    if (ev.type !== 'rtt') refresh();
  }
}

async function voiceLoop(r) {
  for (;;) {
    const p = await r.recv_voice();
    if (!p || r !== room) return;
    post({ t: 'pkt', b: p }, [p.buffer]);
  }
}

async function exitRoom(reason) {
  const r = room;
  room = null;
  state = null;
  joined.clear();
  held.clear();
  cards.clear();
  $('#channels').replaceChildren();
  stopAudio();
  setWakeLock(false);
  $('#room-view').hidden = true;
  $('#join-view').hidden = false;
  if (reason) showJoinError(reason);
  if (r && !leaving) { try { await r.leave(); } catch {} }
}

$('#leave-btn').addEventListener('click', async () => {
  leaving = true;
  const r = room;
  await exitRoom('');
  try { await r?.leave(); } catch {}
});

$('#invite-btn').addEventListener('click', async () => {
  if (!room) return;
  const link = location.origin + location.pathname + '#' + room.ticket();
  const b = $('#invite-btn'), label = b.querySelector('span');
  try {
    await navigator.clipboard.writeText(link);
    log('已複製邀請連結');
    b.classList.add('copied');
    b.querySelector('use').setAttribute('href', '#i-check');
    label.textContent = '已複製';
    clearTimeout(b.timer);
    b.timer = setTimeout(() => {
      b.classList.remove('copied');
      b.querySelector('use').setAttribute('href', '#i-link');
      label.textContent = '複製邀請';
    }, 2000);
  } catch { log('無法複製，請手動複製網址列'); }
});

$('#new-channel').addEventListener('submit', (e) => {
  e.preventDefault();
  const name = $('#new-channel-name').value.trim();
  if (!name || !room) return;
  room.create_channel(name);
  $('#new-channel-name').value = '';
});

function joinChannel(id) {
  const ch = state.channels.find((c) => c.id === id);
  if (!ch) return;
  const pref = chanPrefs()[ch.name] ?? {};
  // 加入頻道就是在收聽（不想聽就離開頻道），跟桌面版一樣不另外提供「收聽」開關
  // 「一直開麥」已拿掉：舊版存的 open 改成偵測說話
  const mode = pref.mode && pref.mode !== 'open' ? pref.mode : 'vad';
  const cfg = { mode, listen: true, volume: pref.volume ?? 1 };
  try {
    room.subscribe(id, cfg.listen, cfg.mode !== 'off');
    joined.set(id, cfg);
  } catch (ex) { log(String(ex.message || ex)); }
  refresh();
}

/** 房主把我移進（只收聽，不改我存的說話方式）或移出頻道；之後我可以自己再改 */
function onMoved(ev) {
  const has = joined.has(ev.channel);
  if (ev.join && !has) {
    const ch = state?.channels.find((c) => c.id === ev.channel);
    const pref = ch ? chanPrefs()[ch.name] ?? {} : {};
    try {
      room.subscribe(ev.channel, true, false);
      joined.set(ev.channel, { mode: 'off', listen: true, volume: pref.volume ?? 1 });
    } catch (ex) { log(String(ex.message || ex)); return; }
  } else if (!ev.join && has) {
    joined.delete(ev.channel);
    held.delete(ev.channel);
    room.unsubscribe(ev.channel);
  } else return;
  log(ev.text);
}

function leaveChannel(id) {
  joined.delete(id);
  held.delete(id);
  room.unsubscribe(id);
  refresh();
}

function updateChannel(id, change) {
  const cfg = joined.get(id);
  const ch = state.channels.find((c) => c.id === id);
  if (!cfg || !ch) return;
  const before = { ...cfg };
  Object.assign(cfg, change);
  if ((before.mode === 'off') !== (cfg.mode === 'off')) room.subscribe(id, cfg.listen, cfg.mode !== 'off');
  if (cfg.mode !== 'ptt') held.delete(id);
  savePrefs(ch.name, cfg, 'mode' in change);
  refresh();
}

/** 成員列表：每人一列（我排第一），下面列出他加入的頻道；只收聽的寫「只聽」（不只靠顏色） */
function renderMembers() {
  $('#members-title').textContent = `成員（${state.members.length}）`;
  const talking = audioStatus?.speaking;
  const list = [...state.members].sort((a, b) => (b.me ? 1 : 0) - (a.me ? 1 : 0) || a.idx - b.idx);
  $('#member-list').replaceChildren(...list.map((m) => {
    const li = document.createElement('li');
    const row = document.createElement('div');
    row.className = 'member-row';
    const dot = document.createElement('span');
    const speaking = !m.me && talking?.has(m.idx);
    dot.className = speaking ? 'dot ok' : 'dot';
    const name = document.createElement('span');
    name.className = 'name';
    name.textContent = m.me ? `${m.name}（我）` : m.name;
    row.append(dot, name);
    if (state.owner === m.idx) {
      row.insertAdjacentHTML('beforeend', '<svg class="ic" role="img" aria-label="房主"><title>房主</title><use href="#i-crown"/></svg>');
    }
    const chans = document.createElement('div');
    chans.className = 'member-chans';
    for (const ch of state.channels) {
      const sub = ch.members.find((x) => x.idx === m.idx);
      if (!sub) continue;
      const b = document.createElement('span');
      b.className = 'badge';
      b.textContent = sub.listen && !sub.talk ? `${ch.name}・只聽` : ch.name;
      b.title = b.textContent;
      chans.append(b);
    }
    if (!chans.childElementCount) {
      const none = document.createElement('span');
      none.className = 'muted small';
      none.textContent = '沒有加入頻道';
      chans.append(none);
    }
    li.append(row, chans);
    return li;
  }));
}

/** Re-read the room state and update everything */
function refresh() {
  if (!room) return;
  state = JSON.parse(room.state());
  // channels that disappeared
  for (const id of [...joined.keys()]) if (!state.channels.some((c) => c.id === id)) { joined.delete(id); held.delete(id); }
  syncAudio();
  syncGains();
  renderRoom();
}

// ---------------------------------------------------------------- per-member volume
// slider: first half 0-100 %, second half 100-1000 % (a linear 0-1000 slider makes 0-100 % hard to set)
const posToGain = (p) => (p <= 0.5 ? p * 2 : 1 + (p - 0.5) * 18);
const gainToPos = (g) => (g <= 1 ? g / 2 : Math.min(1, 0.5 + (g - 1) / 18));
const pct = (g) => `${Math.round(g * 20) * 5}%`;

function syncGains() {
  if (!state) return;
  const want = new Map();
  for (const m of state.members) {
    if (m.me) continue;
    const g = memberVolumes[m.name];
    if (g !== undefined && g !== 1) want.set(m.idx, g);
  }
  for (const [idx, g] of want) if (sentGains.get(idx) !== g) post({ t: 'ugain', idx, gain: g });
  for (const idx of sentGains.keys()) if (!want.has(idx)) post({ t: 'ugain', idx, gain: -1 });
  sentGains = want;
}

let volTarget = null;

function openVolume(idx) {
  if (!state || idx === state.me) return;
  const name = memberName(idx);
  volTarget = name;
  $('#vol-name').textContent = `${name} 的音量`;
  $('#vol-range').value = Math.round(gainToPos(memberVolumes[name] ?? 1) * 1000);
  paintRange($('#vol-range'));
  showVolume();
  $('#vol-pop').hidden = false;
}

function showVolume() {
  const g = posToGain(Number($('#vol-range').value) / 1000);
  $('#vol-val').textContent = pct(g);
}

function setVolume(g) {
  if (!volTarget) return;
  if (Math.abs(g - 1) < 0.005) delete memberVolumes[volTarget];
  else memberVolumes[volTarget] = Math.round(g * 100) / 100;
  store.set('memberVolumes', memberVolumes);
  $('#vol-range').value = Math.round(gainToPos(g) * 1000);
  paintRange($('#vol-range'));
  showVolume();
  syncGains();
  renderRoom();
}

$('#vol-range').addEventListener('input', () => setVolume(posToGain(Number($('#vol-range').value) / 1000)));
$('#vol-mute').addEventListener('click', () => setVolume(0));
$('#vol-reset').addEventListener('click', () => setVolume(1));
$('#vol-done').addEventListener('click', () => { $('#vol-pop').hidden = true; volTarget = null; });
$('#vol-pop').addEventListener('click', (e) => { if (e.target.id === 'vol-pop') $('#vol-done').click(); });

/** long-press (phones) or right-click (computers) a member name to set their volume */
function bindVolumeGestures(list) {
  let timer = 0, start = null;
  const cancel = () => { clearTimeout(timer); timer = 0; };
  list.addEventListener('contextmenu', (e) => {
    const li = e.target.closest('li[data-idx]');
    if (!li) return;
    e.preventDefault();
    cancel();
    openVolume(Number(li.dataset.idx));
  });
  list.addEventListener('pointerdown', (e) => {
    const li = e.target.closest('li[data-idx]');
    if (!li || e.pointerType === 'mouse') return;
    start = [e.clientX, e.clientY];
    cancel();
    timer = setTimeout(() => { timer = 0; openVolume(Number(li.dataset.idx)); }, 500);
  });
  list.addEventListener('pointermove', (e) => {
    if (timer && start && Math.hypot(e.clientX - start[0], e.clientY - start[1]) > 10) cancel();
  });
  list.addEventListener('pointerup', cancel);
  list.addEventListener('pointercancel', cancel);
}

// ---------------------------------------------------------------- rendering
/** 房主的皇冠圖示（Lucide crown） */
function crownIcon() {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'ic owner-ic');
  svg.setAttribute('role', 'img');
  svg.setAttribute('aria-label', '房主');
  const title = document.createElementNS('http://www.w3.org/2000/svg', 'title');
  title.textContent = '房主';
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', '#i-crown');
  svg.append(title, use);
  return svg;
}

function memberName(idx) {
  return state.members.find((m) => m.idx === idx)?.name ?? `#${idx}`;
}

function renderRoom() {
  if (!state) return;
  const dot = $('#conn-dot');
  const via = state.path ? (state.path.relay ? '經轉送' : '直連') : '';
  if (state.recovering) { dot.className = 'dot warn'; $('#conn-text').textContent = '正在改連新房主…'; }
  else {
    dot.className = 'dot ok';
    const rtt = state.rtt != null ? ` · ${Math.round(state.rtt)} ms` : '';
    $('#conn-text').textContent = `已連線（${via}${rtt}）`;
  }
  $('#member-count').textContent = `${state.members.length} 人在房間`;
  renderMembers();
  // 房主是誰（所有人都看得到）：皇冠圖示 + 名字；不在房間裡時皇冠變灰。圖示沒有文字，用 aria-label／title 說明
  const ownerName = state.owner == null ? '' : state.owner === state.me ? '你' : memberName(state.owner);
  const ownerLabel = state.owner == null ? '房主不在房間裡' : `房主：${ownerName}`;
  $('#owner-name').textContent = ownerName;
  $('#owner-badge').setAttribute('aria-label', ownerLabel);
  $('#owner-badge').title = ownerLabel;
  $('#owner-badge').classList.toggle('absent', state.owner == null);

  const seen = new Set();
  const list = $('#channels');
  state.channels.forEach((ch, i) => {
    seen.add(ch.id);
    let card = cards.get(ch.id);
    if (!card) { card = makeCard(ch.id); cards.set(ch.id, card); }
    if (list.children[i] !== card.el) list.insertBefore(card.el, list.children[i] ?? null);
    updateCard(card, ch);
  });
  for (const [id, card] of cards) if (!seen.has(id)) { card.el.remove(); cards.delete(id); }
  renderAudio();
}

function makeCard(id) {
  const el = $('#channel-tpl').content.firstElementChild.cloneNode(true);
  const q = (s) => el.querySelector(s);
  const card = { el, name: q('.ch-name'), count: q('.ch-count'), join: q('.join'), chips: q('.chips'),
    toggle: q('.members-toggle'), summary: q('.speaking-summary'),
    mode: q('.mode'), vol: q('.vol'), volVal: q('.vol-val'), ptt: q('.ptt') };
  card.join.addEventListener('click', () => (joined.has(id) ? leaveChannel(id) : joinChannel(id)));
  bindVolumeGestures(card.chips);
  card.toggle.addEventListener('click', () => {
    if (!expanded.delete(id)) expanded.add(id);
    renderRoom();
  });
  card.mode.addEventListener('change', () => updateChannel(id, { mode: card.mode.value }));
  card.vol.addEventListener('input', () => {
    const cfg = joined.get(id);
    if (!cfg) return;
    cfg.volume = Number(card.vol.value);
    card.volVal.textContent = Math.round(cfg.volume * 100) + '%';
    syncAudio();
  });
  card.vol.addEventListener('change', () => updateChannel(id, { volume: Number(card.vol.value) }));
  // push-to-talk: pointer capture so sliding off the button does not get stuck
  const press = (e) => {
    e.preventDefault();
    try { card.ptt.setPointerCapture(e.pointerId); } catch {}
    held.add(id);
    card.ptt.classList.add('held');
    syncAudio();
  };
  const release = () => {
    if (!held.delete(id)) return;
    card.ptt.classList.remove('held');
    syncAudio();
  };
  card.ptt.addEventListener('pointerdown', press);
  card.ptt.addEventListener('pointerup', release);
  card.ptt.addEventListener('pointercancel', release);
  card.ptt.addEventListener('lostpointercapture', release);
  card.ptt.addEventListener('contextmenu', (e) => e.preventDefault());
  return card;
}

function updateCard(card, ch) {
  const cfg = joined.get(ch.id);
  card.el.classList.toggle('joined', !!cfg);
  card.el.classList.toggle('mode-ptt', cfg?.mode === 'ptt');
  card.name.textContent = ch.name;
  const open = expanded.has(ch.id);
  card.el.classList.toggle('show-members', open);
  card.toggle.textContent = open ? '收起成員' : '顯示成員';
  card.toggle.hidden = ch.members.length === 0;
  card.count.textContent = ch.members.length ? `${ch.members.length} 人` : '';
  card.join.textContent = cfg ? '離開' : '加入';
  card.chips.replaceChildren(...ch.members.map((m) => {
    const li = document.createElement('li');
    li.dataset.idx = m.idx;
    const name = memberName(m.idx);
    li.textContent = name;
    if (m.idx === state.me) li.classList.add('me');
    // 房主：名字後面加皇冠圖示（有 aria-label／title，不只靠顏色）
    if (m.idx === state.owner) li.append(crownIcon());
    const g = m.idx === state.me ? undefined : memberVolumes[name];
    const tag = [!m.talk ? '只聽' : !m.listen ? '不聽' : '', g !== undefined && g !== 1 ? pct(g) : ''].filter(Boolean).join(' ');
    if (tag) { const t = document.createElement('span'); t.className = 'tag'; t.textContent = tag; li.append(t); }
    return li;
  }));
  if (cfg) {
    if (document.activeElement !== card.mode) card.mode.value = cfg.mode;
    if (document.activeElement !== card.vol) { card.vol.value = cfg.volume; paintRange(card.vol); }
    card.volVal.textContent = Math.round(cfg.volume * 100) + '%';
  }
}

/** Speaking highlights, mic meter, stats (called ~10 times per second) */
function renderAudio() {
  if (!state) return;
  const st = audioStatus;
  for (const ch of state.channels) {
    const card = cards.get(ch.id);
    if (!card) continue;
    const bit = ch.slot != null ? 1n << BigInt(ch.slot) : 0n;
    const meTalking = !!(st && st.active && (st.tx & bit));
    card.el.classList.toggle('talking', meTalking);
    const talkers = [];
    for (const li of card.chips.children) {
      const idx = Number(li.dataset.idx);
      let on = false;
      if (idx === state.me) on = meTalking;
      else if (st && bit) { const via = st.speaking.get(idx); on = via !== undefined && (via & bit) !== 0n; }
      li.classList.toggle('speaking', on);
      if (on) talkers.push(memberName(idx));
    }
    card.summary.textContent = talkers.length ? '說話中：' + talkers.join('、') : '';
  }
  const pos = (db) => Math.max(0, Math.min(100, ((db + 70) / 70) * 100));
  $('#meter-gate').style.left = pos(settings.gate) + '%';
  if (st) {
    const fill = $('#meter-fill');
    // 麥克風關閉（靜音／拒聽）或沒有麥克風時音量條不動
    const micOff = !micStream || muted || deafened;
    fill.style.width = (micOff ? 0 : pos(st.level)) + '%';
    fill.classList.toggle('voiced', !micOff && st.voiced);
    const to = state.channels.filter((c) => c.slot != null && (st.tx & (1n << BigInt(c.slot)))).map((c) => c.name);
    const paused = ctx && ctx.state !== 'running';
    const micPaused = micStream?.getAudioTracks()[0]?.muted;
    $('#mic-text').textContent = paused ? '聲音暫停中（回到這個頁面就會恢復）'
      : !micStream ? (micWanted ? '麥克風：已中斷'
        : micProblem === 'none' ? '沒有麥克風（只能聽）'
        : micProblem === 'unsupported' ? '這個瀏覽器不能用麥克風（只能聽）'
        : '麥克風：沒有權限（只能聽）')
      : micPaused ? '麥克風：系統暫停中'
      : muted || deafened ? '麥克風：已關閉'
      : st.active ? `送出中 → ${to.join('、')}` : st.voiced ? '偵測到說話' : '麥克風：安靜';
    const total = st.decoded + st.fec + st.plc;
    const loss = total ? (((st.fec + st.plc) / total) * 100).toFixed(1) : '0';
    $('#net-text').textContent = `收到 ${st.decoded} 幀，補償 ${st.fec + st.plc}（${loss}%），太晚 ${st.late}`;
  }
}

function log(text) {
  const li = document.createElement('li');
  li.textContent = `${new Date().toLocaleTimeString()} ${text}`;
  const l = $('#log');
  l.prepend(li);
  while (l.children.length > 6) l.lastChild.remove();
}

// ---------------------------------------------------------------- top controls & settings
function renderToggles() {
  const m = $('#mute-btn'), d = $('#deafen-btn');
  m.setAttribute('aria-pressed', String(muted || deafened));
  m.querySelector('.tg-label').textContent = muted || deafened ? '麥克風已關' : '麥克風開';
  d.setAttribute('aria-pressed', String(deafened));
  d.querySelector('.tg-label').textContent = deafened ? '拒聽中' : '收聽中';
}
// the room view leaves room at the bottom for the fixed dock (its height changes when a warning shows)
new ResizeObserver(([e]) => {
  document.documentElement.style.setProperty('--dock-h', `${e.target.offsetHeight}px`);
}).observe($('.dock'));
// 寬螢幕（電腦全螢幕）時音訊設定在右邊側欄，空間夠，預設展開
if (matchMedia('(min-width: 1024px)').matches) document.querySelectorAll('.settings').forEach((d) => { d.open = true; });
$('#mute-btn').addEventListener('click', () => {
  // un-muting while deafened also un-deafens (same as desktop)
  if (muted || deafened) { muted = false; deafened = false; } else muted = true;
  renderToggles(); pushSettings();
  // turning the mic on without one: try to open it again (e.g. plugged in, or permission changed)
  if (!muted && ctx && !micStream && micProblem !== 'unsupported') enableMic();
});
$('#deafen-btn').addEventListener('click', () => { deafened = !deafened; renderToggles(); pushSettings(); });

/** Sliders: the filled part of the track follows the value (style.css --fill) */
function paintRange(el) {
  const min = Number(el.min || 0), max = Number(el.max || 100);
  el.style.setProperty('--fill', `${((Number(el.value) - min) / (max - min || 1)) * 100}%`);
}
document.addEventListener('input', (e) => { if (e.target instanceof HTMLInputElement && e.target.type === 'range') paintRange(e.target); });

function bindSetting(id, key, fmt, apply) {
  const el = $('#' + id);
  const isCheck = el.type === 'checkbox';
  if (isCheck) el.checked = settings[key]; else { el.value = settings[key]; if (el.type === 'range') paintRange(el); }
  const show = () => { if (fmt) $('#' + id + '-val').textContent = fmt(settings[key]); };
  show();
  el.addEventListener(isCheck ? 'change' : 'input', () => {
    settings[key] = isCheck ? el.checked : Number(el.value);
    store.set(key, settings[key]);
    show();
    apply?.(settings[key]);
  });
}
bindSetting('volume', 'volume', (v) => Math.round(v * 100) + '%', pushSettings);
bindSetting('gate', 'gate', (v) => `${v} dB`, () => { pushSettings(); renderAudio(); });
bindSetting('denoise', 'denoise', null, applyDenoise);
bindSetting('cues', 'cues', null, () => { pushSettings(); renderCueVolume(); });
bindSetting('cue-volume', 'cueVolume', (v) => Math.round(v * 100) + '%', pushSettings);
// release the slider (or the preview button) -> play the cue once at the new volume
$('#cue-volume').addEventListener('change', () => post({ t: 'previewCue' }));
$('#cue-preview').addEventListener('click', () => post({ t: 'previewCue' }));
function renderCueVolume() { $('#cue-volume-field').hidden = !settings.cues; }
renderCueVolume();
bindSetting('wakelock', 'wakelock', null, (on) => setWakeLock(on && !!room));
renderToggles();

async function setWakeLock(on) {
  try {
    if (on && room && 'wakeLock' in navigator && document.visibilityState === 'visible') {
      wakeLock ??= await navigator.wakeLock.request('screen');
      wakeLock.addEventListener('release', () => { wakeLock = null; }, { once: true });
    } else if (!on && wakeLock) {
      await wakeLock.release();
      wakeLock = null;
    }
  } catch {}
}
/** Back in the foreground: resume audio, and re-open the mic if the OS stopped or paused it */
async function recoverAudio() {
  if (!room || !ctx) return;
  setAudioSession('play-and-record');
  try { await ctx.resume(); } catch {}
  const t = micStream?.getAudioTracks()[0];
  if (micWanted && (!t || t.readyState === 'ended' || t.muted)) {
    // give iOS a moment to un-pause the track by itself
    await new Promise((r) => setTimeout(r, 600));
    const t2 = micStream?.getAudioTracks()[0];
    if (!t2 || t2.readyState === 'ended' || t2.muted) await enableMic();
  }
  renderAudio();
}
function setAudioSession(type) {
  try { if (navigator.audioSession) navigator.audioSession.type = type; } catch {}
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') {
    setWakeLock(settings.wakelock);
    recoverAudio();
  }
});
window.addEventListener('pageshow', () => recoverAudio());
window.addEventListener('pagehide', () => { room?.leave(); });
setInterval(() => { if (room) refresh(); }, 2000);

if ('serviceWorker' in navigator && location.protocol === 'https:') {
  navigator.serviceWorker.register('sw.js').catch(() => {});
}
