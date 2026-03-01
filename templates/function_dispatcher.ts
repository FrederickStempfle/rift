/**
 * Rift Function Dispatcher (auto-generated)
 *
 * True serverless: each request gets a fresh V8 isolate via a new Web Worker.
 * No shared state between invocations. Concurrent requests run in parallel
 * Workers, up to MAX_CONCURRENT per function.
 *
 * __ROUTE_COUNT__ function route(s) detected.
 */

const PORT = parseInt(Deno.env.get("PORT") ?? "3000");
const MAX_CONCURRENT = parseInt(Deno.env.get("RIFT_MAX_CONCURRENT") ?? "50");

interface Route {
  pattern: URLPattern;
  workerPath: string;
  active: number;
}

interface WorkerRequest {
  url: string;
  method: string;
  headers: [string, string][];
  body: number[] | null;
}

interface WorkerResponse {
  status: number;
  headers: [string, string][];
  body: number[] | null;
  error?: string;
}

// Route table — injected at build time
const routes: Route[] = __ROUTES__;

// Metrics
let totalInvocations = 0;
let coldStarts = 0;

/**
 * Dispatch a single request to a fresh Worker.
 * The Worker is created, handles exactly one request, then is terminated.
 * This guarantees clean global state per invocation.
 */
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
    coldStarts++;

    let settled = false;
    const worker = new Worker(route.workerPath, {
      type: "module",
      deno: { permissions: "inherit" },
    } as WorkerOptions);

    // Timeout: kill Worker if it takes too long (30s default)
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

      // Terminate the Worker — clean slate for next invocation
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

      console.error(`[rift-dispatcher] Worker error: ${e.message}`);
      resolve(new Response(
        JSON.stringify({ error: "Worker Error" }),
        { status: 500, headers: { "content-type": "application/json" } },
      ));
    };

    try {
      worker.postMessage(data);
    } catch (e) {
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

  // Internal metrics endpoint
  if (url.pathname === "/__rift/metrics") {
    return new Response(JSON.stringify({
      total_invocations: totalInvocations,
      cold_starts: coldStarts,
      routes: routes.map(r => ({
        pattern: r.pattern.pathname,
        active: r.active,
      })),
    }), { headers: { "content-type": "application/json" } });
  }

  for (const route of routes) {
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
    });
  }

  return new Response(
    JSON.stringify({ error: "Not Found" }),
    { status: 404, headers: { "content-type": "application/json" } },
  );
});
