// MultiVoice audio engine inside an AudioWorklet (see crates/web-audio).
// wasm-bindgen glue does not work in AudioWorkletGlobalScope (no TextDecoder etc.),
// so the engine exports plain C ABI functions and we touch its memory directly.
//
// main -> worklet messages: {t:'wasm', bytes} {t:'pkt', b} {t:'slot', slot, active, volume, pan, priority}
//   {t:'talk', open:[lo,hi], vad:[lo,hi]} {t:'ptt', m:[lo,hi]} {t:'flags', muted, deafened, cues}
//   {t:'volume', v} {t:'gate', db} {t:'denoise', on}
// worklet -> main: {t:'ready'} {t:'error', msg} {t:'out', b:ArrayBuffer} {t:'status', s:Int32Array}

const STATUS_EVERY = 4800; // samples (100 ms)

class MvEngine extends AudioWorkletProcessor {
  constructor() {
    super();
    this.x = null;
    this.h = 0;
    this.queue = [];
    this.hadInput = false;
    this.sinceStatus = 0;
    this.port.onmessage = (e) => this.onMessage(e.data);
  }

  onMessage(m) {
    if (m.t === 'wasm') {
      WebAssembly.instantiate(m.bytes, {}).then(({ instance }) => {
        this.x = instance.exports;
        this.h = this.x.mv_new();
        for (const q of this.queue) this.onMessage(q);
        this.queue = [];
        this.port.postMessage({ t: 'ready' });
      }, (err) => this.port.postMessage({ t: 'error', msg: String(err) }));
      return;
    }
    if (!this.x) { this.queue.push(m); return; }
    const x = this.x, h = this.h;
    switch (m.t) {
      case 'pkt': {
        const p = x.mv_packet_buf(h, m.b.length);
        new Uint8Array(x.memory.buffer, p, m.b.length).set(m.b);
        x.mv_push_packet(h, m.b.length);
        break;
      }
      case 'slot': x.mv_set_slot(h, m.slot, m.active ? 1 : 0, m.volume, m.pan, m.priority ? 1 : 0); break;
      case 'talk': x.mv_set_talk(h, m.open[0], m.open[1], m.vad[0], m.vad[1]); break;
      case 'ptt': x.mv_set_ptt(h, m.m[0], m.m[1]); break;
      case 'flags': x.mv_set_flags(h, m.muted ? 1 : 0, m.deafened ? 1 : 0, m.cues ? 1 : 0); break;
      case 'volume': x.mv_set_volume(h, m.v); break;
      case 'gate': x.mv_set_gate(h, m.db); break;
      case 'denoise': x.mv_set_denoise(h, m.on ? 1 : 0); break;
      case 'ugain': x.mv_set_user_gain(h, m.idx, m.gain); break;
    }
  }

  process(inputs, outputs) {
    const out = outputs[0];
    const n = out[0].length;
    if (!this.x) return true;
    const x = this.x, h = this.h;
    const mic = inputs[0] && inputs[0][0];
    const has = !!(mic && mic.length === n);
    if (has) new Float32Array(x.memory.buffer, x.mv_input(h), n).set(mic);
    else if (this.hadInput) x.mv_mic_stopped(h);
    this.hadInput = has;
    x.mv_process(h, n, has ? 1 : 0);
    // memory.buffer may have been replaced by growth: create views after the call
    out[0].set(new Float32Array(x.memory.buffer, x.mv_out_l(h), n));
    if (out[1]) out[1].set(new Float32Array(x.memory.buffer, x.mv_out_r(h), n));

    const len = x.mv_outbox_len(h);
    if (len) {
      const b = new Uint8Array(x.memory.buffer, x.mv_outbox(h), len).slice().buffer;
      x.mv_outbox_clear(h);
      this.port.postMessage({ t: 'out', b }, [b]);
    }
    this.sinceStatus += n;
    if (this.sinceStatus >= STATUS_EVERY) {
      this.sinceStatus = 0;
      const p = x.mv_status(h);
      const count = new Int32Array(x.memory.buffer, p, 1)[0];
      const s = new Int32Array(x.memory.buffer, p, count).slice();
      this.port.postMessage({ t: 'status', s }, [s.buffer]);
    }
    return true;
  }
}

registerProcessor('mv-engine', MvEngine);
