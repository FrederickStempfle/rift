/**
 * Rift Function Worker (auto-generated)
 *
 * Runs a single function handler inside a Web Worker isolate.
 * Receives requests via postMessage and returns responses the same way.
 *
 * __BUNDLE_IMPORT__ is replaced at build time with the actual bundle path.
 */

// @ts-ignore — replaced at build time
import * as mod from "__BUNDLE_IMPORT__";

type Handler = (req: Request) => Response | Promise<Response>;

function resolveHandler(m: Record<string, unknown>): Handler | null {
  const d = m.default as Record<string, unknown> | ((...args: unknown[]) => unknown) | undefined;
  if (d && typeof (d as Record<string, unknown>).fetch === "function") {
    return ((d as Record<string, unknown>).fetch as Handler).bind(d);
  }
  if (typeof d === "function") return d as Handler;
  if (typeof m.fetch === "function") return m.fetch as Handler;
  if (typeof m.handler === "function") return m.handler as Handler;
  return null;
}

const handler = resolveHandler(mod as Record<string, unknown>);

interface WorkerRequest {
  url: string;
  method: string;
  headers: [string, string][];
  body: number[] | null;
  envVars?: Record<string, string>;
}

interface WorkerResponse {
  status: number;
  headers: [string, string][];
  body: number[] | null;
  error?: string;
}

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const { url, method, headers, body, envVars } = e.data;

  // Inject per-project env vars (safe — each Worker is a fresh isolate)
  if (envVars) {
    for (const [k, v] of Object.entries(envVars)) {
      Deno.env.set(k, v);
    }
  }

  if (!handler) {
    const resp: WorkerResponse = {
      status: 500,
      headers: [["content-type", "application/json"]],
      body: Array.from(new TextEncoder().encode(
        JSON.stringify({ error: "No handler found" }),
      )),
    };
    self.postMessage(resp);
    return;
  }

  try {
    const req = new Request(url, {
      method,
      headers: new Headers(headers),
      body: body ? new Uint8Array(body) : undefined,
    });

    const resp = await handler(req);
    const respBody = new Uint8Array(await resp.arrayBuffer());
    const respHeaders: [string, string][] = [...resp.headers.entries()];

    const msg: WorkerResponse = {
      status: resp.status,
      headers: respHeaders,
      body: Array.from(respBody),
    };
    self.postMessage(msg);
  } catch (e) {
    console.error(`[rift-worker] Handler error: ${e}`);
    const msg: WorkerResponse = {
      status: 500,
      headers: [["content-type", "application/json"]],
      body: Array.from(new TextEncoder().encode(
        JSON.stringify({ error: "Internal Server Error" }),
      )),
      error: String(e),
    };
    self.postMessage(msg);
  }
};
