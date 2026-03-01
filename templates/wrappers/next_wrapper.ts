/**
 * Rift Pool Entry Wrapper for Next.js
 *
 * Starts the Next.js standalone server on an internal port and proxies
 * requests to it. This allows the worker pool to manage the lifecycle
 * while Next.js runs its own HTTP server internally.
 */

const INTERNAL_PORT = 9999;

// Start Next.js server as a subprocess
const serverJsPath = Deno.env.get("RIFT_NEXT_SERVER_JS") ?? "./server.js";
const serverDir = Deno.env.get("RIFT_NEXT_SERVER_DIR") ?? ".";

const proc = new Deno.Command("node", {
  args: [serverJsPath],
  cwd: serverDir,
  env: {
    ...Object.fromEntries(
      Array.from(Object.entries(Deno.env.toObject())).filter(
        ([k]) => !k.startsWith("RIFT_"),
      ),
    ),
    PORT: String(INTERNAL_PORT),
    HOSTNAME: "127.0.0.1",
    NODE_ENV: "production",
  },
  stdout: "piped",
  stderr: "piped",
}).spawn();

// Wait for Next.js to start
async function waitForReady(port: number, maxAttempts = 60): Promise<void> {
  for (let i = 0; i < maxAttempts; i++) {
    try {
      const conn = await Deno.connect({ hostname: "127.0.0.1", port });
      conn.close();
      return;
    } catch {
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  throw new Error(`Next.js server did not start on port ${port}`);
}

await waitForReady(INTERNAL_PORT);

// Export a fetch handler that proxies to the internal Next.js server
export default {
  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    const targetUrl = `http://127.0.0.1:${INTERNAL_PORT}${url.pathname}${url.search}`;

    const headers = new Headers(req.headers);
    headers.delete("host");

    const resp = await fetch(targetUrl, {
      method: req.method,
      headers,
      body: req.body,
      redirect: "manual",
    });

    return new Response(resp.body, {
      status: resp.status,
      statusText: resp.statusText,
      headers: resp.headers,
    });
  },
};
