// Rift Function Isolate — Web Standards Shim
//
// Loaded into every V8 isolate (baked into the startup snapshot).
// Provides the minimal Web Standards API surface that function handlers need:
// Headers, Request, Response, fetch(), console, Deno.env, process.env, crypto.randomUUID().

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

  // --- crypto.randomUUID ---
  globalThis.crypto = globalThis.crypto || {};
  globalThis.crypto.randomUUID = () =>
    Deno.core.ops.op_rift_crypto_random_uuid();

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
