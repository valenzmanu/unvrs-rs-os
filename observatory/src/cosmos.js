// Cosmos engine host. The picture is decided in Rust (cosmos.wasm): simulation, shaders,
// buffers, render targets and every pass. This file only (1) hands the module a WebGL2
// context through the "gl" import table, one forwarding function per call, (2) runs the
// frame clock and adaptive quality, and (3) relays page messages. Runs inside a Worker on
// an OffscreenCanvas when the browser allows it, otherwise on the page.
'use strict';
(function (G) {
  const PHASES = ['FIRST LIGHT', 'APPROACH', 'FIRST PASSAGE', 'TIDAL TAILS', 'BLACK HOLE BINARY',
    'MERGER · GRAVITATIONAL WAVE', 'REMNANT', 'DUSK'];
  // frame intervals kept for the p95 in the stats (about four seconds at 60 fps)
  const DT_RING = 240;
  // 3840×2160 at 1:1 fits; above that (retina 4K) the render is scaled to this budget
  const MAX_RENDER_PX = 8.4e6;

  // The import table. Handles are indexes into small arrays; 0 means "none".
  function glImports(eng) {
    const H = { prog: [null], loc: [null], buf: [null], vao: [null], tgt: [null] };
    const put = (a, v) => (a.push(v), a.length - 1);
    const dec = new TextDecoder();
    const mem = () => eng.x.memory.buffer;
    const str = (p, n) => dec.decode(new Uint8Array(mem(), p, n));
    const gl = () => eng.gl;
    const shader = (type, src) => {
      const g = gl(), s = g.createShader(type);
      g.shaderSource(s, src); g.compileShader(s);
      if (!g.getShaderParameter(s, g.COMPILE_STATUS)) { console.warn('cosmos shader:', g.getShaderInfoLog(s)); return null; }
      return s;
    };
    return {
      gl_log: (p, n) => console.warn('cosmos:', str(p, n)),
      gl_program: (vp, vl, fp, fl) => {
        const g = gl();
        const vs = shader(g.VERTEX_SHADER, str(vp, vl)), fs = shader(g.FRAGMENT_SHADER, str(fp, fl));
        if (!vs || !fs) return 0;
        const p = g.createProgram();
        g.attachShader(p, vs); g.attachShader(p, fs); g.linkProgram(p);
        if (!g.getProgramParameter(p, g.LINK_STATUS)) { console.warn('cosmos link:', g.getProgramInfoLog(p)); return 0; }
        return put(H.prog, p);
      },
      gl_uniform: (p, np, nl) => {
        const l = gl().getUniformLocation(H.prog[p], str(np, nl));
        return l ? put(H.loc, l) : 0;
      },
      gl_buffer: () => put(H.buf, { b: gl().createBuffer(), cap: 0 }),
      gl_buffer_data: (b, p, n) => {
        const g = gl(), e = H.buf[b];
        g.bindBuffer(g.ARRAY_BUFFER, e.b);
        if (n > e.cap) { e.cap = Math.ceil(n * 1.25); g.bufferData(g.ARRAY_BUFFER, e.cap, g.DYNAMIC_DRAW); }
        if (n) g.bufferSubData(g.ARRAY_BUFFER, 0, new Uint8Array(mem(), p, n));
      },
      gl_vao: () => put(H.vao, gl().createVertexArray()),
      gl_attrib: (v, b, loc, size, ty, stride, off, div) => {
        const g = gl();
        g.bindVertexArray(H.vao[v]);
        g.bindBuffer(g.ARRAY_BUFFER, H.buf[b].b);
        g.enableVertexAttribArray(loc);
        g.vertexAttribPointer(loc, size, ty ? g.UNSIGNED_BYTE : g.FLOAT, !!ty, stride, off);
        g.vertexAttribDivisor(loc, div);
        g.bindVertexArray(null);
      },
      gl_target: (w, h, hdr) => {
        const g = gl(), t = g.createTexture();
        g.bindTexture(g.TEXTURE_2D, t);
        const f16 = hdr && eng.float;
        g.texImage2D(g.TEXTURE_2D, 0, f16 ? g.RGBA16F : g.RGBA8, w, h, 0, g.RGBA, f16 ? g.HALF_FLOAT : g.UNSIGNED_BYTE, null);
        g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MIN_FILTER, g.LINEAR);
        g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MAG_FILTER, g.LINEAR);
        g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_S, g.CLAMP_TO_EDGE);
        g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_T, g.CLAMP_TO_EDGE);
        const fb = g.createFramebuffer();
        g.bindFramebuffer(g.FRAMEBUFFER, fb);
        g.framebufferTexture2D(g.FRAMEBUFFER, g.COLOR_ATTACHMENT0, g.TEXTURE_2D, t, 0);
        const ok = g.checkFramebufferStatus(g.FRAMEBUFFER) === g.FRAMEBUFFER_COMPLETE;
        g.bindFramebuffer(g.FRAMEBUFFER, null);
        if (!ok) { g.deleteTexture(t); g.deleteFramebuffer(fb); return 0; }
        return put(H.tgt, { t, fb });
      },
      gl_free_target: (id) => {
        const e = H.tgt[id];
        if (e) { gl().deleteTexture(e.t); gl().deleteFramebuffer(e.fb); H.tgt[id] = null; }
      },
      gl_bind_target: (id, w, h) => {
        const g = gl();
        g.bindFramebuffer(g.FRAMEBUFFER, id ? H.tgt[id].fb : null);
        g.viewport(0, 0, w, h);
      },
      gl_use: (p) => gl().useProgram(H.prog[p]),
      gl_bind_vao: (v) => gl().bindVertexArray(H.vao[v]),
      gl_texture: (unit, id) => {
        const g = gl();
        g.activeTexture(g.TEXTURE0 + unit);
        g.bindTexture(g.TEXTURE_2D, id ? H.tgt[id].t : null);
      },
      gl_u1i: (l, a) => l && gl().uniform1i(H.loc[l], a),
      gl_u1f: (l, a) => l && gl().uniform1f(H.loc[l], a),
      gl_u2f: (l, a, b) => l && gl().uniform2f(H.loc[l], a, b),
      gl_u3f: (l, a, b, c) => l && gl().uniform3f(H.loc[l], a, b, c),
      gl_u4f: (l, a, b, c, d) => l && gl().uniform4f(H.loc[l], a, b, c, d),
      gl_blend: (m) => {
        const g = gl();
        if (m) { g.enable(g.BLEND); g.blendFunc(g.ONE, g.ONE); } else g.disable(g.BLEND);
      },
      gl_draw: (mode, first, count, inst) => {
        const g = gl(), m = mode ? g.TRIANGLE_STRIP : g.TRIANGLES;
        if (inst > 1 || mode) g.drawArraysInstanced(m, first, count, inst); else g.drawArrays(m, first, count);
      },
    };
  }

  function Engine(post) {
    this.post = post;
    this.flags = { visible: true, onscreen: true, focused: true };
    this.reduced = false;
    this.x = null;
    this.raf = 0;
    this.last = 0;
    this.emaMs = 4;
    this.emaDt = 16.7;
    this.frames = 0;
    this.statAt = 0;
    this.fpsFrames = 0;
    this.queue = [];
    this.quality = 1;
    this.slow = 0;
    this.win = null;
    this.dts = new Float32Array(DT_RING);
    this.ndt = 0;
  }

  Engine.prototype.msg = function (m) {
    if (m.type === 'init') return this.init(m);
    if (!this.x) { this.queue.push(m); return; }
    switch (m.type) {
      case 'resize': this.resize(m.w, m.h, m.dpr); break;
      case 'run': Object.assign(this.flags, m); this.schedule(); break;
      case 'reduced': this.reduced = !!m.on; this.x.set_still(this.reduced ? 1 : 0); if (this.reduced) this.still(); this.schedule(); break;
      case 'signals': this.x.signals(m.workers >>> 0, m.calls >>> 0); if (this.reduced) this.draw(0); break;
      case 'comet': this.x.comet(); break;
      case 'exposure': this.x.set_exposure(+m.k || 1); break;
      case 'window': this.win = m; this.x.set_window(m.x0, m.y0, m.x1, m.y1); if (this.reduced) this.draw(0); break;
      case 'focus': this.x.set_focus(m.x0, m.y0, m.x1, m.y1); if (this.reduced) this.draw(0); break;
    }
  };

  Engine.prototype.init = async function (m) {
    this.canvas = m.canvas;
    this.reduced = !!m.reduced;
    this.gl = this.canvas.getContext('webgl2', {
      alpha: false, antialias: false, depth: false, stencil: false,
      premultipliedAlpha: false, preserveDrawingBuffer: false, desynchronized: true, powerPreference: 'high-performance',
    });
    if (!this.gl) { this.post({ type: 'error', text: 'cosmos: WebGL2 is not available' }); return; }
    this.float = !!this.gl.getExtension('EXT_color_buffer_float');
    const dbg = this.gl.getExtension('WEBGL_debug_renderer_info');
    this.renderer = dbg ? this.gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : this.gl.getParameter(this.gl.RENDERER);
    let inst;
    try {
      const res = await fetch(m.wasm);
      const imports = { gl: glImports(this) };
      inst = WebAssembly.instantiateStreaming
        ? (await WebAssembly.instantiateStreaming(res, imports)).instance
        : (await WebAssembly.instantiate(await res.arrayBuffer(), imports)).instance;
    } catch (e) {
      this.post({ type: 'error', text: 'cosmos: ' + e });
      return;
    }
    this.x = inst.exports;
    if (!this.x.gpu_init()) { this.post({ type: 'error', text: 'cosmos: shaders failed' }); this.x = null; return; }
    this.x.set_still(this.reduced ? 1 : 0);
    this.resize(m.w, m.h, m.dpr);
    const q = this.queue; this.queue = [];
    q.forEach((x) => this.msg(x));
    this.post({ type: 'ready', renderer: this.renderer, float: this.float });
    if (this.reduced) this.still();
    this.schedule();
  };

  Engine.prototype.resize = function (cw, ch, dpr) {
    cw = Math.max(16, cw | 0); ch = Math.max(16, ch | 0);
    this.css = [cw, ch, dpr];
    const area = cw * ch;
    const small = area < 200000;
    const scale = Math.min(dpr || 1, 2, Math.sqrt(MAX_RENDER_PX / area)) * this.quality;
    const w = Math.max(16, Math.round(cw * scale)), h = Math.max(16, Math.round(ch * scale));
    const cap = Math.round(Math.min(640000, Math.max(60000, area * 1.7)) / 4) * 4;
    this.small = small;
    this.budget = small ? 5.0 : 9.0;
    if (!this.cap || cap > this.cap * 1.5) {
      this.cap = this.x.init(cap, (Math.random() * 4294967295) >>> 0);
      this.presimmed = false;
      this.n = Math.min(this.cap, this.small ? 90000 : 260000);
    }
    this.n = this.x.set_count(Math.min(this.n, this.cap));
    this.x.set_exposure(this.small ? 1.15 : 1.0);
    this.w = w; this.h = h;
    this.canvas.width = w; this.canvas.height = h;
    this.x.resize(w, h);
    if (this.reduced) this.still(); else this.draw(0);
  };

  Engine.prototype.running = function () {
    const f = this.flags;
    return !!this.x && !this.reduced && f.visible && f.onscreen;
  };

  Engine.prototype.schedule = function () {
    if (this.raf || !this.running()) return;
    const raf = G.requestAnimationFrame ? G.requestAnimationFrame.bind(G) : (cb) => setTimeout(() => cb(performance.now()), 16);
    const tick = (now) => {
      this.raf = 0;
      if (!this.running()) { this.last = 0; return; }
      this.raf = raf(tick);
      // an unfocused panel (the captain is typing in the thread beside it) runs at 30 fps
      const minDt = this.flags.focused ? 0 : 1000 / 30 - 2;
      if (this.last && now - this.last < minDt) return;
      const had = this.last;
      const dt = had ? Math.min(now - had, 100) : 16.7;
      this.last = now;
      this.emaDt += (dt - this.emaDt) * 0.05;
      if (had) this.dts[this.ndt++ % DT_RING] = now - had;
      this.draw(dt / 1000);
      this.adapt(now);
    };
    this.raf = raf(tick);
  };

  // Quality: first the star count (CPU: simulation), then the render scale (GPU: fill).
  Engine.prototype.adapt = function (now) {
    if (++this.frames % 30 === 0) {
      const target = this.flags.focused ? 16.7 : 33.3;
      let n = this.n;
      if (this.emaMs > this.budget * 1.1) n = n * 0.85;
      else if (this.emaMs < this.budget * 0.6 && this.emaDt < target * 1.15) n = n * 1.08;
      n = Math.max(40000, Math.min(this.cap, n));
      if (Math.abs(n - this.n) > 2000) this.n = this.x.set_count(n);
      // frames arrive late while the CPU side is cheap: the GPU is the bottleneck
      if (this.emaDt > target * 1.35 && this.emaMs < this.budget) this.slow++;
      else this.slow = Math.max(0, this.slow - 1);
      if (this.slow >= 4 && this.quality > 0.5) {
        this.quality = Math.max(0.5, this.quality * 0.85);
        this.slow = 0;
        this.resize(...this.css);
      }
    }
    this.fpsFrames++;
    if (now - this.statAt > 500) {
      const fps = this.statAt ? (this.fpsFrames * 1000) / (now - this.statAt) : 0;
      this.statAt = now; this.fpsFrames = 0;
      this.stats(fps);
    }
  };

  // 95th percentile of the recent frame intervals (ms), 0 before there are enough
  Engine.prototype.p95 = function () {
    const n = Math.min(this.ndt, DT_RING);
    if (n < 30) return 0;
    const a = Array.from(this.dts.subarray(0, n)).sort((x, y) => x - y);
    return +a[Math.min(n - 1, Math.floor(n * 0.95))].toFixed(2);
  };

  Engine.prototype.stats = function (fps) {
    const info = new Float32Array(this.x.memory.buffer, this.x.info(), 32);
    this.post({ type: 'stats', phase: PHASES[info[0] | 0] || '', epoch: info[14] | 0,
      stars: this.n, fps: Math.round(fps), ms: this.emaMs, dt: this.emaDt, p95: this.p95(), w: this.w, h: this.h,
      quality: this.quality, renderer: this.renderer, float: this.float, still: this.reduced });
  };

  Engine.prototype.still = function () {
    if (!this.x) return;
    if (!this.presimmed) { this.x.presim(48); this.presimmed = true; }
    this.draw(0);
    this.stats(0);
  };

  Engine.prototype.draw = function (dt) {
    const t0 = performance.now();
    this.x.frame(dt);
    if (dt > 0) this.emaMs += (performance.now() - t0 - this.emaMs) * 0.08;
  };

  const inWorker = typeof WorkerGlobalScope !== 'undefined' && G instanceof WorkerGlobalScope;
  if (inWorker) {
    let eng = null;
    G.onmessage = (e) => {
      if (!eng) eng = new Engine((m) => G.postMessage(m));
      eng.msg(e.data);
    };
  } else {
    G.CosmosEngine = Engine;
  }
})(self);
