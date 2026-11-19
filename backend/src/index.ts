import { handleMe, handleLicenseRefresh, handleStoreThemes } from "./api.js";
import { handleAuthCallback, handleAuthStart } from "./auth.js";
import {
  applyCors,
  corsPreflight,
  isPreflight,
  rejectDisallowedOrigin,
} from "./cors.js";
import { config, type Env } from "./env.js";
import { ApiError, errorResponse, json, methodNotAllowed } from "./errors.js";
import { jwksResponse } from "./tokens.js";
import { handleBoostyWebhook } from "./webhooks.js";

function requireMethod(request: Request, method: string): void {
  if (request.method !== method) {
    throw methodNotAllowed([method]);
  }
}

async function route(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const path = url.pathname.length > 1 ? url.pathname.replace(/\/+$/, "") : url.pathname;

  if (path === "/health") {
    requireMethod(request, "GET");
    return json({ ok: true, version: config.version(env) });
  }

  if (path === "/auth/discord/start") {
    requireMethod(request, "GET");
    return await handleAuthStart(env, url);
  }

  if (path === "/auth/discord/callback") {
    requireMethod(request, "GET");
    return await handleAuthCallback(env, url);
  }

  if (path === "/api/keys/jwks") {
    requireMethod(request, "GET");
    return await jwksResponse(env);
  }

  if (path === "/api/me") {
    requireMethod(request, "GET");
    return await handleMe(env, request);
  }

  if (path === "/api/license/refresh") {
    requireMethod(request, "POST");
    return await handleLicenseRefresh(env, request);
  }

  if (path === "/api/store/themes") {
    requireMethod(request, "GET");
    return await handleStoreThemes(env, request);
  }

  if (path === "/webhooks/boosty") {
    requireMethod(request, "POST");
    return await handleBoostyWebhook(env, request);
  }

  throw new ApiError(404, "not_found", "Route not found");
}

export default {
  async fetch(request: Request, env: Env, _ctx: ExecutionContext): Promise<Response> {
    try {
      if (isPreflight(request)) return corsPreflight(request);

      const rejected = rejectDisallowedOrigin(request);
      if (rejected) return rejected;

      return applyCors(request, await route(request, env));
    } catch (error) {
      return applyCors(request, errorResponse(error));
    }
  },
} satisfies ExportedHandler<Env>;

export { route };
