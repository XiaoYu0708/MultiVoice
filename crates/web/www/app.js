// MultiVoice web client: UI + glue between the network (wasm-bindgen module `web`)
// and the audio engine running in an AudioWorklet (`worklet.js` + `web_audio.wasm`).
import init, * as mv from './pkg/web.js';

const $ = (s) => document.querySelector(s);
const store = {
  get(k, d) { try { const v = localStorage.getItem('mv.' + k); return v === null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem('mv.' + k, JSON.stringify(v)); } catch {} },
};
const MODE_LABEL = { vad: '偵測說話', open: '一直開麥', ptt: '按住說話', off: '不說話' };
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
  cues: store.get('cues', true),
  wakelock: store.get('wakelock', true),
};
/** channel id -> { mode, listen, volume } for channels I joined */
const joined = new Map();
/** channel ids whose PTT button is held */
const held = new Set();
const cards = new Map();
/** channel ids whose member list is expanded (collapsed by default) */
const expanded = new Set();
let sentSlots = new Map();     // slot -> JSON of what was sent to the worklet

const chanPrefs = () => store.get('channels', {});
function savePrefs(name, cfg) {
  const all = chanPrefs();
  all[name] = { mode: cfg.mode, volume: cfg.volume };
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
  if (err) return showJoinError('邀請票格式錯誤：' + err);
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

async function startAudio() {
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
  pushSettings();
  await enableMic();
}

/** Open the microphone (also used to re-open it after the OS cut it off in the background) */
async function enableMic() {
  micStream?.getTracks().forEach((t) => t.stop());
  micStream = null;
  try { micSource?.disconnect(); } catch {}
  micSource = null;
  try {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
    });
    if (!ctx || !node) { stream.getTracks().forEach((t) => t.stop()); return; }
    micStream = stream;
    micWanted = true;
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
  } catch {
    showMicWarning('沒有麥克風權限，只能聽。', true);
  }
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
  try { node?.disconnect(); } catch {}
  node = null;
  ctx?.close().catch(() => {});
  ctx = null;
}

function post(m, transfer) { node?.port.postMessage(m, transfer); }

function pushSettings() {
  post({ t: 'volume', v: settings.volume });
  post({ t: 'gate', db: settings.gate });
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
    if (ev.type === 'log' || ev.type === 'error') log(ev.text);
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
  try { await navigator.clipboard.writeText(link); log('已複製邀請連結'); }
  catch { log('無法複製，請手動複製網址列'); }
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
  const cfg = { mode: pref.mode ?? 'vad', listen: true, volume: pref.volume ?? 1 };
  try {
    room.subscribe(id, cfg.listen, cfg.mode !== 'off');
    joined.set(id, cfg);
  } catch (ex) { log(String(ex.message || ex)); }
  refresh();
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
  savePrefs(ch.name, cfg);
  refresh();
}

/** Re-read the room state and update everything */
function refresh() {
  if (!room) return;
  state = JSON.parse(room.state());
  // channels that disappeared
  for (const id of [...joined.keys()]) if (!state.channels.some((c) => c.id === id)) { joined.delete(id); held.delete(id); }
  syncAudio();
  renderRoom();
}

// ---------------------------------------------------------------- rendering
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
    li.textContent = memberName(m.idx);
    if (m.idx === state.me) li.classList.add('me');
    const tag = !m.talk ? '只聽' : !m.listen ? '不聽' : '';
    if (tag) { const t = document.createElement('span'); t.className = 'tag'; t.textContent = tag; li.append(t); }
    return li;
  }));
  if (cfg) {
    if (document.activeElement !== card.mode) card.mode.value = cfg.mode;
    if (document.activeElement !== card.vol) card.vol.value = cfg.volume;
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
    fill.style.width = (micStream ? pos(st.level) : 0) + '%';
    fill.classList.toggle('voiced', st.voiced);
    const to = state.channels.filter((c) => c.slot != null && (st.tx & (1n << BigInt(c.slot)))).map((c) => c.name);
    const paused = ctx && ctx.state !== 'running';
    const micPaused = micStream?.getAudioTracks()[0]?.muted;
    $('#mic-text').textContent = paused ? '聲音暫停中（回到這個頁面就會恢復）'
      : !micStream ? (micWanted ? '麥克風：已中斷' : '麥克風：沒有權限（只能聽）')
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
  m.textContent = muted || deafened ? '麥克風已關' : '麥克風開';
  d.setAttribute('aria-pressed', String(deafened));
  d.textContent = deafened ? '拒聽中' : '收聽中';
}
$('#mute-btn').addEventListener('click', () => {
  // un-muting while deafened also un-deafens (same as desktop)
  if (muted || deafened) { muted = false; deafened = false; } else muted = true;
  renderToggles(); pushSettings();
});
$('#deafen-btn').addEventListener('click', () => { deafened = !deafened; renderToggles(); pushSettings(); });

function bindSetting(id, key, fmt, apply) {
  const el = $('#' + id);
  const isCheck = el.type === 'checkbox';
  if (isCheck) el.checked = settings[key]; else el.value = settings[key];
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
bindSetting('cues', 'cues', null, pushSettings);
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
