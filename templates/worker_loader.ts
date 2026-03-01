/**
 * Rift Worker Loader
 *
 * Pre-warmed Deno worker that listens for specialization commands.
 * When specialized, it dynamically imports the user's entry point
 * and forwards all HTTP requests to it.
 *
 * Control endpoints:
 *   POST /__rift/specialize  — Load a deployment bundle
 *   GET  /__rift/health      — Readiness check
 *   POST /__rift/unspecialize — Unload user code (returns to warm state)
 */

const SERVE_PORT = parseInt(Deno.env.get("RIFT_SERVE_PORT") ?? "0");

type Handler = (req: Request) => Response | Promise<Response>;

let userHandler: Handler | null = null;
let specialized = false;
let deploymentId: string | null = null;
let projectId: string | null = null;

// Track per-request metrics
let requestCount = 0;

Deno.serve({ port: SERVE_PORT, hostname: "127.0.0.1" }, async (req) => {
  const url = new URL(req.url);

  // --- Control Plane ---

  if (url.pathname === "/__rift/specialize" && req.method === "POST") {
    try {
      const body = await req.json();
      const { bundle_path, env_vars, deployment_id, project_id: pid } = body;

      // Inject environment variables
      if (env_vars && typeof env_vars === "object") {
        for (const [key, value] of Object.entries(env_vars)) {
          Deno.env.set(key, value as string);
        }
      }

      // Dynamically import the user's entry point
      const mod = await import(`file://${bundle_path}`);

      // Support multiple export conventions:
      //   export default { fetch(req) {} }        — Cloudflare Workers style
      //   export default function handler(req) {}  — Simple default export
      //   export function fetch(req) {}            — Named export
      //   export { handler as default }            — Re-export
      if (mod.default?.fetch && typeof mod.default.fetch === "function") {
        userHandler = mod.default.fetch.bind(mod.default);
      } else if (typeof mod.default === "function") {
        userHandler = mod.default;
      } else if (typeof mod.fetch === "function") {
        userHandler = mod.fetch;
      } else if (typeof mod.handler === "function") {
        userHandler = mod.handler;
      } else {
        return new Response(
          JSON.stringify({ error: "No handler found. Export a default function or { fetch } handler." }),
          { status: 400, headers: { "content-type": "application/json" } },
        );
      }

      specialized = true;
      deploymentId = deployment_id ?? null;
      projectId = pid ?? null;
      requestCount = 0;

      return new Response(
        JSON.stringify({ status: "specialized", deployment_id: deploymentId }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    } catch (e) {
      return new Response(
        JSON.stringify({ error: `Specialization failed: ${e}` }),
        { status: 500, headers: { "content-type": "application/json" } },
      );
    }
  }

  if (url.pathname === "/__rift/health") {
    const memUsage = Deno.memoryUsage();
    return new Response(
      JSON.stringify({
        state: specialized ? "specialized" : "warm",
        deployment_id: deploymentId,
        project_id: projectId,
        request_count: requestCount,
        memory: {
          rss: memUsage.rss,
          heap_used: memUsage.heapUsed,
          heap_total: memUsage.heapTotal,
        },
      }),
      { status: 200, headers: { "content-type": "application/json" } },
    );
  }

  // --- User Request Forwarding ---

  if (!specialized || !userHandler) {
    return new Response("Worker not specialized", { status: 503 });
  }

  requestCount++;

  try {
    return await userHandler(req);
  } catch (e) {
    console.error(`[rift-worker] Handler error: ${e}`);
    return new Response(`Internal Server Error`, { status: 500 });
  }
});
