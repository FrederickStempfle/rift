/**
 * Rift Global Function Dispatcher
 *
 * A single always-running Deno process that handles ALL projects' function
 * invocations. Routes are registered dynamically via HTTP control endpoints.
 * Each request gets a fresh V8 isolate via a new Web Worker — true serverless.
 *
 * Control plane:
 *   POST   /_rift/register              Register/update a project's function routes
 *   DELETE /_rift/unregister/:projectId  Remove a project's routes
 *   GET    /_rift/health                 Liveness check
 *   GET    /_rift/metrics                Stats
 */

const PORT = parseInt(Deno.env.get("PORT") ?? "9999");
const MAX_CONCURRENT = parseInt(Deno.env.get("RIFT_MAX_CONCURRENT") ?? "50");

interface Route {
  pattern: URLPattern;
  workerPath: string;
  active: number;
}

interface ProjectEntry {
  projectId: string;
  routes: Route[];
  envVars: Record<string, string>;
}

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

// Project registry — updated via control endpoints
const projects = new Map<string, ProjectEntry>();

// Metrics
let totalInvocations = 0;
let totalColdStarts = 0;

function dispatch(route: Route, data: WorkerRequest): Promise<Response> {
  return new Promise<Response>((resolve) => {
    if (route.active >= MAX_CONCURRENT) {
      resolve(new Response(
        JSON.stringify({ error: "Too Many Requests" }),
        { status: 429, headers: { "content-type": "application/json" } },
      ));
      return;
    }

    route.active++;
    totalInvocations++;
    totalColdStarts++;

    let settled = false;
    const worker = new Worker(route.workerPath, {
      type: "module",
      deno: {
        permissions: {
          net: true,
          read: true,
          env: true,
          write: false,
          run: false,
          ffi: false,
          sys: false,
        },
      },
    } as WorkerOptions);

    const timeout = setTimeout(() => {
      if (!settled) {
        settled = true;
        route.active--;
        worker.terminate();
        resolve(new Response(
          JSON.stringify({ error: "Function timed out" }),
          { status: 504, headers: { "content-type": "application/json" } },
        ));
      }
    }, 30_000);

    worker.onmessage = (e: MessageEvent<WorkerResponse>) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      route.active--;

      const resp = e.data;
      const body = resp.body ? new Uint8Array(resp.body) : null;
      worker.terminate();

      if (resp.error) {
        resolve(new Response(
          JSON.stringify({ error: "Internal Server Error" }),
          { status: resp.status || 500, headers: { "content-type": "application/json" } },
        ));
        return;
      }

      resolve(new Response(body, {
        status: resp.status,
        headers: new Headers(resp.headers),
      }));
    };

    worker.onerror = (e: ErrorEvent) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      route.active--;
      worker.terminate();

      console.error(`[rift-global-dispatcher] Worker error: ${e.message}`);
      resolve(new Response(
        JSON.stringify({ error: "Worker Error" }),
        { status: 500, headers: { "content-type": "application/json" } },
      ));
    };

    try {
      worker.postMessage(data);
    } catch (_e) {
      if (!settled) {
        settled = true;
        clearTimeout(timeout);
        route.active--;
        worker.terminate();
        resolve(new Response(
          JSON.stringify({ error: "Failed to dispatch to worker" }),
          { status: 500, headers: { "content-type": "application/json" } },
        ));
      }
    }
  });
}

Deno.serve({ port: PORT, hostname: "0.0.0.0" }, async (req) => {
  const url = new URL(req.url);

  // --- Control Plane ---

  if (url.pathname === "/_rift/register" && req.method === "POST") {
    try {
      const body = await req.json();
      const { projectId, routes, envVars } = body as {
        projectId: string;
        routes: { pattern: string; workerPath: string }[];
        envVars: Record<string, string>;
      };

      const compiledRoutes: Route[] = routes.map((r) => ({
        pattern: new URLPattern({ pathname: r.pattern }),
        workerPath: r.workerPath,
        active: 0,
      }));

      projects.set(projectId, {
        projectId,
        routes: compiledRoutes,
        envVars: envVars ?? {},
      });

      console.log(`[rift-global-dispatcher] Registered project ${projectId} with ${routes.length} route(s)`);
      return new Response(JSON.stringify({ ok: true }), {
        headers: { "content-type": "application/json" },
      });
    } catch (e) {
      return new Response(
        JSON.stringify({ error: `Registration failed: ${e}` }),
        { status: 400, headers: { "content-type": "application/json" } },
      );
    }
  }

  if (url.pathname.startsWith("/_rift/unregister/") && req.method === "DELETE") {
    const projectId = url.pathname.slice("/_rift/unregister/".length);
    const existed = projects.delete(projectId);
    console.log(`[rift-global-dispatcher] Unregistered project ${projectId} (existed: ${existed})`);
    return new Response(JSON.stringify({ ok: true, existed }), {
      headers: { "content-type": "application/json" },
    });
  }

  if (url.pathname === "/_rift/health") {
    return new Response(JSON.stringify({ status: "ok", projects: projects.size }), {
      headers: { "content-type": "application/json" },
    });
  }

  if (url.pathname === "/_rift/metrics") {
    const projectMetrics: Record<string, unknown> = {};
    for (const [pid, entry] of projects) {
      projectMetrics[pid] = entry.routes.map((r) => ({
        pattern: r.pattern.pathname,
        active: r.active,
      }));
    }
    return new Response(JSON.stringify({
      total_invocations: totalInvocations,
      cold_starts: totalColdStarts,
      registered_projects: projects.size,
      projects: projectMetrics,
    }), { headers: { "content-type": "application/json" } });
  }

  // --- Data Plane ---

  const projectId = req.headers.get("x-rift-project-id");
  if (!projectId) {
    return new Response(
      JSON.stringify({ error: "Missing x-rift-project-id header" }),
      { status: 400, headers: { "content-type": "application/json" } },
    );
  }

  const project = projects.get(projectId);
  if (!project) {
    return new Response(
      JSON.stringify({ error: "Project not registered" }),
      { status: 404, headers: { "content-type": "application/json" } },
    );
  }

  for (const route of project.routes) {
    const match = route.pattern.exec(url);
    if (!match) continue;

    const groups = match.pathname.groups;
    const headers: [string, string][] = [...req.headers.entries()];
    for (const [k, v] of Object.entries(groups)) {
      if (v !== undefined) headers.push([`x-rift-param-${k}`, v as string]);
    }

    const body = req.body
      ? Array.from(new Uint8Array(await req.arrayBuffer()))
      : null;

    return dispatch(route, {
      url: req.url,
      method: req.method,
      headers,
      body,
      envVars: project.envVars,
    });
  }

  return new Response(
    JSON.stringify({ error: "Not Found" }),
    { status: 404, headers: { "content-type": "application/json" } },
  );
});
