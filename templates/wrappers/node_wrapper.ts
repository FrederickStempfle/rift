/**
 * Rift Pool Entry Wrapper for Node.js SSR Servers
 *
 * Works with Nuxt, Astro, SvelteKit, and Remix.
 * Starts the Node.js server on an internal port and proxies requests.
 */

const INTERNAL_PORT = 9999;

const entryPath = Deno.env.get("RIFT_NODE_ENTRY") ?? "./index.js";
const serverDir = Deno.env.get("RIFT_NODE_SERVER_DIR") ?? ".";
const isRemix = Deno.env.get("RIFT_IS_REMIX") === "true";

// Build environment without RIFT_ prefixed vars
const cleanEnv: Record<string, string> = {};
for (const [k, v] of Object.entries(Deno.env.toObject())) {
  if (!k.startsWith("RIFT_")) {
    cleanEnv[k] = v;
  }
}
cleanEnv["PORT"] = String(INTERNAL_PORT);
cleanEnv["HOST"] = "127.0.0.1";
cleanEnv["NODE_ENV"] = "production";
cleanEnv["NITRO_PORT"] = String(INTERNAL_PORT);
cleanEnv["NITRO_HOST"] = "127.0.0.1";

// Start the server
const cmd = isRemix ? "npx" : "node";
const args = isRemix ? ["remix-serve", entryPath] : [entryPath];

const proc = new Deno.Command(cmd, {
  args,
  cwd: serverDir,
  env: cleanEnv,
  stdout: "piped",
  stderr: "piped",
}).spawn();

// Wait for the server to start
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
  throw new Error(`Node server did not start on port ${port}`);
}

await waitForReady(INTERNAL_PORT);

// Export a fetch handler that proxies to the internal server
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
