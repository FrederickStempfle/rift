// Rift Function Isolate — Web Standards Shim
//
// Loaded into every V8 isolate (baked into the startup snapshot).
// Provides the Web Standards API surface that function handlers need:
// URL, URLSearchParams, Headers, Request, Response, fetch(), AbortController,
// AbortSignal, Event, EventTarget, Blob, FormData, TextEncoder, TextDecoder,
// structuredClone, atob/btoa, queueMicrotask, console, crypto, setTimeout,
// Deno.env, process.env.

((globalThis) => {
  "use strict";

  // --- Headers ---
  class Headers {
    constructor(init) {
      this._map = new Map();
      if (init instanceof Headers) {
        for (const [k, v] of init) {
          this._map.set(k.toLowerCase(), v);
        }
      } else if (Array.isArray(init)) {
        for (const [k, v] of init) {
          this._map.set(k.toLowerCase(), String(v));
        }
      } else if (init && typeof init === "object") {
        for (const [k, v] of Object.entries(init)) {
          this._map.set(k.toLowerCase(), String(v));
        }
      }
    }
    get(name) {
      return this._map.get(name.toLowerCase()) ?? null;
    }
    set(name, value) {
      this._map.set(name.toLowerCase(), String(value));
    }
    has(name) {
      return this._map.has(name.toLowerCase());
    }
    delete(name) {
      this._map.delete(name.toLowerCase());
    }
    append(name, value) {
      const lower = name.toLowerCase();
      const existing = this._map.get(lower);
      this._map.set(lower, existing ? `${existing}, ${value}` : String(value));
    }
    entries() {
      return this._map.entries();
    }
    keys() {
      return this._map.keys();
    }
    values() {
      return this._map.values();
    }
    forEach(cb) {
      this._map.forEach((v, k) => cb(v, k, this));
    }
    [Symbol.iterator]() {
      return this._map.entries();
    }
  }
  globalThis.Headers = Headers;

  // --- Request ---
  class Request {
    constructor(input, init = {}) {
      if (typeof input === "string") {
        this._url = input;
      } else if (input instanceof Request) {
        this._url = input.url;
        init = {
          method: input.method,
          headers: input.headers,
          body: input._body,
          ...init,
        };
      } else {
        this._url = String(input);
      }
      this.method = (init.method || "GET").toUpperCase();
      this.headers = new Headers(init.headers);
      this.url = this._url;

      if (init.body === null || init.body === undefined) {
        this._body = null;
      } else if (init.body instanceof Uint8Array) {
        this._body = init.body;
      } else if (typeof init.body === "string") {
        this._body = new TextEncoder().encode(init.body);
      } else {
        this._body = new TextEncoder().encode(String(init.body));
      }
    }
    async text() {
      if (this._body === null) return "";
      return new TextDecoder().decode(this._body);
    }
    async json() {
      return JSON.parse(await this.text());
    }
    async arrayBuffer() {
      if (this._body === null) return new ArrayBuffer(0);
      return this._body.buffer.slice(
        this._body.byteOffset,
        this._body.byteOffset + this._body.byteLength,
      );
    }
    get body() {
      return this._body;
    }
  }
  globalThis.Request = Request;

  // --- Response ---
  class Response {
    constructor(body, init = {}) {
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.headers = new Headers(init.headers);
      this.ok = this.status >= 200 && this.status < 300;

      if (body === null || body === undefined) {
        this._body = null;
      } else if (body instanceof Uint8Array) {
        this._body = body;
      } else if (typeof body === "string") {
        this._body = new TextEncoder().encode(body);
        if (!this.headers.has("content-type")) {
          this.headers.set("content-type", "text/plain;charset=UTF-8");
        }
      } else if (body instanceof ArrayBuffer) {
        this._body = new Uint8Array(body);
      } else {
        this._body = new TextEncoder().encode(String(body));
      }
    }
    async text() {
      if (this._body === null) return "";
      return new TextDecoder().decode(this._body);
    }
    async json() {
      return JSON.parse(await this.text());
    }
    async arrayBuffer() {
      if (this._body === null) return new ArrayBuffer(0);
      return this._body.buffer.slice(
        this._body.byteOffset,
        this._body.byteOffset + this._body.byteLength,
      );
    }
    get body() {
      return this._body;
    }
    static json(data, init = {}) {
      const body = JSON.stringify(data);
      const headers = new Headers(init.headers);
      if (!headers.has("content-type")) {
        headers.set("content-type", "application/json");
      }
      return new Response(body, { ...init, headers });
    }
    static redirect(url, status = 302) {
      return new Response(null, {
        status,
        headers: { location: String(url) },
      });
    }
  }
  globalThis.Response = Response;

  // --- fetch() ---
  globalThis.fetch = async function fetch(input, init = {}) {
    const req = input instanceof Request ? input : new Request(input, init);
    const headersArray = [...req.headers.entries()];
    // Convert Uint8Array body to regular Array for serde serialization
    const bodyArray = req._body ? Array.from(req._body) : null;
    const result = await Deno.core.ops.op_rift_fetch(
      req.url,
      req.method,
      headersArray,
      bodyArray,
    );
    return new Response(
      result.body ? new Uint8Array(result.body) : null,
      { status: result.status, headers: result.headers },
    );
  };

  // --- console ---
  globalThis.console = {
    log(...args) {
      Deno.core.ops.op_rift_console_log(args.map(String).join(" "));
    },
    info(...args) {
      Deno.core.ops.op_rift_console_log(args.map(String).join(" "));
    },
    warn(...args) {
      Deno.core.ops.op_rift_console_log(args.map(String).join(" "));
    },
    error(...args) {
      Deno.core.ops.op_rift_console_error(args.map(String).join(" "));
    },
    debug(...args) {
      Deno.core.ops.op_rift_console_log(args.map(String).join(" "));
    },
  };

  // --- Deno.env ---
  if (!globalThis.Deno) globalThis.Deno = {};
  globalThis.Deno.env = {
    get(key) {
      return Deno.core.ops.op_rift_env_get(key) ?? undefined;
    },
    toObject() {
      return {};
    },
  };

  // --- process.env ---
  globalThis.process = globalThis.process || {};
  globalThis.process.env = new Proxy(
    {},
    {
      get(_, key) {
        if (typeof key !== "string") return undefined;
        return Deno.core.ops.op_rift_env_get(key) ?? undefined;
      },
    },
  );

  // --- URL / URLSearchParams ---
  // V8 provides these natively in deno_core, but ensure they exist on globalThis.
  if (typeof globalThis.URL === "undefined") {
    class URL {
      constructor(url, base) {
        let full = url;
        if (base) {
          if (typeof base === "string") {
            // Naive base resolution: strip trailing path, append
            const baseStr = base.replace(/\/[^/]*$/, "");
            full = url.startsWith("/") ? base.replace(/\/\/[^/]+(\/.*)?$/, (m, p) => m.replace(p || "", "") + url) : `${baseStr}/${url}`;
          }
        }
        const match = full.match(/^(https?:)\/\/([^/:]+)(?::(\d+))?(\/[^?#]*)?\??([^#]*)?\#?(.*)$/);
        if (!match) throw new TypeError(`Invalid URL: ${url}`);
        this.protocol = match[1];
        this.hostname = match[2];
        this.port = match[3] || "";
        this.pathname = match[4] || "/";
        this.search = match[5] ? `?${match[5]}` : "";
        this.hash = match[6] ? `#${match[6]}` : "";
        this.host = this.port ? `${this.hostname}:${this.port}` : this.hostname;
        this.origin = `${this.protocol}//${this.host}`;
        this.href = `${this.origin}${this.pathname}${this.search}${this.hash}`;
        this.searchParams = new URLSearchParams(match[5] || "");
      }
      toString() { return this.href; }
      toJSON() { return this.href; }
    }
    globalThis.URL = URL;
  }

  if (typeof globalThis.URLSearchParams === "undefined") {
    class URLSearchParams {
      constructor(init = "") {
        this._entries = [];
        if (typeof init === "string") {
          const str = init.startsWith("?") ? init.slice(1) : init;
          if (str) {
            for (const pair of str.split("&")) {
              const [k, ...rest] = pair.split("=");
              this._entries.push([decodeURIComponent(k), decodeURIComponent(rest.join("="))]);
            }
          }
        } else if (Array.isArray(init)) {
          for (const [k, v] of init) this._entries.push([String(k), String(v)]);
        } else if (init && typeof init === "object") {
          for (const [k, v] of Object.entries(init)) this._entries.push([k, String(v)]);
        }
      }
      get(name) { const e = this._entries.find(([k]) => k === name); return e ? e[1] : null; }
      getAll(name) { return this._entries.filter(([k]) => k === name).map(([, v]) => v); }
      has(name) { return this._entries.some(([k]) => k === name); }
      set(name, value) {
        let found = false;
        this._entries = this._entries.filter(([k]) => {
          if (k === name && !found) { found = true; return true; }
          return k !== name;
        });
        if (found) { this._entries.find(([k]) => k === name)[1] = String(value); }
        else { this._entries.push([name, String(value)]); }
      }
      append(name, value) { this._entries.push([String(name), String(value)]); }
      delete(name) { this._entries = this._entries.filter(([k]) => k !== name); }
      toString() { return this._entries.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&"); }
      forEach(cb) { for (const [k, v] of this._entries) cb(v, k, this); }
      entries() { return this._entries[Symbol.iterator](); }
      keys() { return this._entries.map(([k]) => k)[Symbol.iterator](); }
      values() { return this._entries.map(([, v]) => v)[Symbol.iterator](); }
      [Symbol.iterator]() { return this.entries(); }
    }
    globalThis.URLSearchParams = URLSearchParams;
  }

  // --- Event / EventTarget ---
  if (typeof globalThis.Event === "undefined") {
    class Event {
      constructor(type, opts = {}) {
        this.type = type;
        this.bubbles = !!opts.bubbles;
        this.cancelable = !!opts.cancelable;
        this.defaultPrevented = false;
        this.target = null;
        this.currentTarget = null;
        this.timeStamp = Date.now();
      }
      preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
      stopPropagation() {}
      stopImmediatePropagation() {}
    }
    globalThis.Event = Event;
  }

  if (typeof globalThis.EventTarget === "undefined") {
    class EventTarget {
      constructor() { this._listeners = new Map(); }
      addEventListener(type, listener) {
        if (!this._listeners.has(type)) this._listeners.set(type, []);
        this._listeners.get(type).push(listener);
      }
      removeEventListener(type, listener) {
        const arr = this._listeners.get(type);
        if (arr) this._listeners.set(type, arr.filter((l) => l !== listener));
      }
      dispatchEvent(event) {
        event.target = this;
        event.currentTarget = this;
        const arr = this._listeners.get(event.type) || [];
        for (const listener of arr) {
          if (typeof listener === "function") listener(event);
          else if (listener && typeof listener.handleEvent === "function") listener.handleEvent(event);
        }
        return !event.defaultPrevented;
      }
    }
    globalThis.EventTarget = EventTarget;
  }

  // --- AbortController / AbortSignal ---
  if (typeof globalThis.AbortController === "undefined") {
    class AbortSignal extends EventTarget {
      constructor() { super(); this.aborted = false; this.reason = undefined; }
      throwIfAborted() { if (this.aborted) throw this.reason; }
      static abort(reason) {
        const signal = new AbortSignal();
        signal.aborted = true;
        signal.reason = reason ?? new DOMException("The operation was aborted.", "AbortError");
        return signal;
      }
      static timeout(ms) {
        const signal = new AbortSignal();
        globalThis.setTimeout(() => {
          signal.aborted = true;
          signal.reason = new DOMException("The operation timed out.", "TimeoutError");
          signal.dispatchEvent(new Event("abort"));
        }, ms);
        return signal;
      }
    }
    class AbortController {
      constructor() { this.signal = new AbortSignal(); }
      abort(reason) {
        if (this.signal.aborted) return;
        this.signal.aborted = true;
        this.signal.reason = reason ?? new DOMException("The operation was aborted.", "AbortError");
        this.signal.dispatchEvent(new Event("abort"));
      }
    }
    globalThis.AbortSignal = AbortSignal;
    globalThis.AbortController = AbortController;
  }

  // --- DOMException (needed by AbortController) ---
  if (typeof globalThis.DOMException === "undefined") {
    class DOMException extends Error {
      constructor(message = "", name = "Error") {
        super(message);
        this.name = name;
        this.code = 0;
      }
    }
    globalThis.DOMException = DOMException;
  }

  // --- Blob ---
  if (typeof globalThis.Blob === "undefined") {
    class Blob {
      constructor(parts = [], options = {}) {
        this.type = options.type || "";
        const chunks = [];
        for (const part of parts) {
          if (typeof part === "string") {
            chunks.push(new TextEncoder().encode(part));
          } else if (part instanceof ArrayBuffer) {
            chunks.push(new Uint8Array(part));
          } else if (part instanceof Uint8Array) {
            chunks.push(part);
          } else if (part instanceof Blob) {
            chunks.push(part._data);
          } else {
            chunks.push(new TextEncoder().encode(String(part)));
          }
        }
        let totalLen = 0;
        for (const c of chunks) totalLen += c.byteLength;
        this._data = new Uint8Array(totalLen);
        let offset = 0;
        for (const c of chunks) { this._data.set(c, offset); offset += c.byteLength; }
        this.size = totalLen;
      }
      async text() { return new TextDecoder().decode(this._data); }
      async arrayBuffer() { return this._data.buffer.slice(this._data.byteOffset, this._data.byteOffset + this._data.byteLength); }
      slice(start = 0, end = this.size, contentType = "") {
        const sliced = this._data.slice(start, end);
        const blob = new Blob([sliced], { type: contentType });
        return blob;
      }
    }
    globalThis.Blob = Blob;
  }

  // --- FormData ---
  if (typeof globalThis.FormData === "undefined") {
    class FormData {
      constructor() { this._entries = []; }
      append(name, value) { this._entries.push([String(name), value]); }
      set(name, value) { this.delete(name); this.append(name, value); }
      get(name) { const e = this._entries.find(([k]) => k === name); return e ? e[1] : null; }
      getAll(name) { return this._entries.filter(([k]) => k === name).map(([, v]) => v); }
      has(name) { return this._entries.some(([k]) => k === name); }
      delete(name) { this._entries = this._entries.filter(([k]) => k !== name); }
      entries() { return this._entries[Symbol.iterator](); }
      keys() { return this._entries.map(([k]) => k)[Symbol.iterator](); }
      values() { return this._entries.map(([, v]) => v)[Symbol.iterator](); }
      forEach(cb) { for (const [k, v] of this._entries) cb(v, k, this); }
      [Symbol.iterator]() { return this.entries(); }
    }
    globalThis.FormData = FormData;
  }

  // --- TextEncoder / TextDecoder ---
  // These are typically provided by V8/deno_core, but ensure they're on globalThis.
  if (typeof globalThis.TextEncoder === "undefined") {
    globalThis.TextEncoder = class TextEncoder {
      get encoding() { return "utf-8"; }
      encode(str = "") {
        const buf = [];
        for (let i = 0; i < str.length; i++) {
          let c = str.charCodeAt(i);
          if (c < 0x80) { buf.push(c); }
          else if (c < 0x800) { buf.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f)); }
          else if (c >= 0xd800 && c <= 0xdbff) {
            const hi = c; const lo = str.charCodeAt(++i);
            c = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
            buf.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
          } else { buf.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f)); }
        }
        return new Uint8Array(buf);
      }
    };
  }
  if (typeof globalThis.TextDecoder === "undefined") {
    globalThis.TextDecoder = class TextDecoder {
      constructor(label = "utf-8") { this.encoding = label; }
      decode(input) {
        if (!input) return "";
        const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
        let str = "", i = 0;
        while (i < bytes.length) {
          const b = bytes[i];
          if (b < 0x80) { str += String.fromCharCode(b); i++; }
          else if ((b & 0xe0) === 0xc0) { str += String.fromCharCode(((b & 0x1f) << 6) | (bytes[i+1] & 0x3f)); i += 2; }
          else if ((b & 0xf0) === 0xe0) { str += String.fromCharCode(((b & 0x0f) << 12) | ((bytes[i+1] & 0x3f) << 6) | (bytes[i+2] & 0x3f)); i += 3; }
          else if ((b & 0xf8) === 0xf0) {
            const cp = ((b & 0x07) << 18) | ((bytes[i+1] & 0x3f) << 12) | ((bytes[i+2] & 0x3f) << 6) | (bytes[i+3] & 0x3f);
            str += String.fromCodePoint(cp); i += 4;
          } else { str += "\ufffd"; i++; }
        }
        return str;
      }
    };
  }

  // --- structuredClone ---
  if (typeof globalThis.structuredClone === "undefined") {
    globalThis.structuredClone = function structuredClone(value) {
      if (value === null || typeof value !== "object") return value;
      return JSON.parse(JSON.stringify(value));
    };
  }

  // --- atob / btoa ---
  if (typeof globalThis.atob === "undefined") {
    const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    globalThis.btoa = function btoa(str) {
      let out = "", i = 0;
      for (; i < str.length - 2; i += 3) {
        const b = (str.charCodeAt(i) << 16) | (str.charCodeAt(i+1) << 8) | str.charCodeAt(i+2);
        out += chars[(b >> 18) & 63] + chars[(b >> 12) & 63] + chars[(b >> 6) & 63] + chars[b & 63];
      }
      if (i === str.length - 1) {
        const b = str.charCodeAt(i) << 16;
        out += chars[(b >> 18) & 63] + chars[(b >> 12) & 63] + "==";
      } else if (i === str.length - 2) {
        const b = (str.charCodeAt(i) << 16) | (str.charCodeAt(i+1) << 8);
        out += chars[(b >> 18) & 63] + chars[(b >> 12) & 63] + chars[(b >> 6) & 63] + "=";
      }
      return out;
    };
    globalThis.atob = function atob(str) {
      const lookup = new Uint8Array(128);
      for (let i = 0; i < chars.length; i++) lookup[chars.charCodeAt(i)] = i;
      str = str.replace(/=+$/, "");
      let out = "", i = 0;
      while (i < str.length) {
        const b = (lookup[str.charCodeAt(i++)] << 18) | (lookup[str.charCodeAt(i++)] << 12) |
                  (lookup[str.charCodeAt(i++) || 0] << 6) | (lookup[str.charCodeAt(i++) || 0]);
        out += String.fromCharCode((b >> 16) & 255);
        if (str.length > i - 2) out += String.fromCharCode((b >> 8) & 255);
        if (str.length > i - 1) out += String.fromCharCode(b & 255);
      }
      return out;
    };
  }

  // --- queueMicrotask ---
  if (typeof globalThis.queueMicrotask === "undefined") {
    globalThis.queueMicrotask = (fn) => Promise.resolve().then(fn);
  }

  // --- crypto.randomUUID + crypto.getRandomValues ---
  globalThis.crypto = globalThis.crypto || {};
  globalThis.crypto.randomUUID = () =>
    Deno.core.ops.op_rift_crypto_random_uuid();
  if (typeof globalThis.crypto.getRandomValues === "undefined") {
    globalThis.crypto.getRandomValues = (array) => {
      // Simple PRNG fallback — the Rust side provides randomUUID via a proper source
      for (let i = 0; i < array.length; i++) {
        array[i] = Math.floor(Math.random() * 256);
      }
      return array;
    };
  }

  // --- setTimeout / clearTimeout (basic) ---
  let _timerId = 0;
  const _timers = new Map();
  globalThis.setTimeout = (fn, ms = 0) => {
    const id = ++_timerId;
    const promise = new Promise((resolve) => {
      Deno.core.ops.op_rift_sleep(ms).then(() => {
        if (_timers.has(id)) {
          _timers.delete(id);
          try { fn(); } catch (_e) { /* ignore */ }
        }
        resolve();
      });
    });
    _timers.set(id, promise);
    return id;
  };
  globalThis.clearTimeout = (id) => {
    _timers.delete(id);
  };
})(globalThis);
