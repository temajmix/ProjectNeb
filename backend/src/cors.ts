import { ApiError, errorResponse, json } from "./errors.js";

/**
 * Browser calls are allowed only from:
 *   - the desktop WebView2 virtual host (`https://neb.ui`)
 *   - a loopback listener on any port (`http://127.0.0.1:<port>` /
 *     `http://localhost:<port>`)
 *
 * Requests without an `Origin` header (CLI, worker-to-worker, desktop native
 * HTTP) are not CORS requests and pass through unchanged.
 */
export const APP_ORIGIN = "https://neb.ui";

const LOOPBACK_ORIGIN_PATTERN = /^http:\/\/(127\.0\.0\.1|localhost):([0-9]{1,5})$/i;

/** Returns the canonical origin when it is allowed, otherwise null. */
export function allowedOrigin(origin: string | null | undefined): string | null {
  if (!origin) return null;
  const value = origin.trim();
  if (value === APP_ORIGIN) return APP_ORIGIN;

  const match = LOOPBACK_ORIGIN_PATTERN.exec(value);
  if (!match) return null;
  const port = Number.parseInt(match[2] as string, 10);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) return null;
  return `http://${(match[1] as string).toLowerCase()}:${port}`;
}

function appendVary(headers: Headers, value: string): void {
  const current = headers.get("vary");
  if (!current) {
    headers.set("vary", value);
    return;
  }
  const entries = current.split(",").map((entry) => entry.trim().toLowerCase());
  if (!entries.includes(value.toLowerCase())) headers.set("vary", `${current}, ${value}`);
}

/**
 * Adds CORS response headers for allowed origins. A browser request carrying a
 * disallowed origin is rejected with 403 before it reaches the router.
 */
export function applyCors(request: Request, response: Response): Response {
  const headers = new Headers(response.headers);
  const origin = request.headers.get("origin");
  if (!origin) return new Response(response.body, { status: response.status, headers });

  appendVary(headers, "Origin");
  const allowed = allowedOrigin(origin);
  if (allowed) {
    headers.set("access-control-allow-origin", allowed);
    headers.set("access-control-expose-headers", "retry-after");
  }
  return new Response(response.body, { status: response.status, headers });
}

export function isPreflight(request: Request): boolean {
  return request.method === "OPTIONS";
}

export function corsPreflight(request: Request): Response {
  const origin = request.headers.get("origin");
  const allowed = allowedOrigin(origin);
  if (!allowed) {
    return json(
      {
        error: {
          code: "origin_not_allowed",
          message: "This origin is not allowed to call the ProjectNeb API",
        },
      },
      { status: 403, headers: { vary: "Origin" } },
    );
  }

  return new Response(null, {
    status: 204,
    headers: {
      "access-control-allow-origin": allowed,
      "access-control-allow-methods": "GET, POST, OPTIONS",
      "access-control-allow-headers": "authorization, content-type",
      "access-control-max-age": "600",
      "access-control-expose-headers": "retry-after",
      vary: "Origin",
      "cache-control": "no-store",
    },
  });
}

/** Rejects a non-preflight browser request from a disallowed origin. */
export function rejectDisallowedOrigin(request: Request): Response | null {
  const origin = request.headers.get("origin");
  if (!origin || allowedOrigin(origin)) return null;
  return applyCors(
    request,
    errorResponse(
      new ApiError(403, "origin_not_allowed", "This origin is not allowed to call the ProjectNeb API"),
    ),
  );
}
