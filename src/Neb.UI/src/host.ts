import { bridge } from "./bridge";
import { createDemoHost } from "./demo";
import type { Metrics } from "./state";

/** The outcome of one host call, shaped so a screen can render a state instead of catching errors. */
export type CallResult<T> =
  | { ok: true; data: T }
  | { ok: false; reason: "unavailable" | "failed"; message: string };

export interface Host {
  /** True in the browser preview, where responses come from `demo.ts`. */
  readonly demo: boolean;
  /** Sample numbers for the preview, or null inside the real host (it pushes `neb.metrics`). */
  previewMetrics(): Metrics | null;
  call<T>(method: string, params?: unknown): Promise<CallResult<T>>;
  send(type: string, payload?: unknown): void;
  on<T>(type: string, handler: (payload: T) => void): () => void;
}

const MissingMethod =
  /unknown|not found|unimplemented|unsupported|no such|missing method|invalid method/i;

/** A host that has never heard of a method answers with an error string; that is not a crash. */
export function isMissingMethod(message: string): boolean {
  return MissingMethod.test(message);
}

export function createHost(): Host {
  // In a plain browser there is no WebView2 port. The standalone build then offers the same window
  // with preview data, so it can be reviewed (and screenshotted) before the shell is running.
  const demo = bridge.available ? null : createDemoHost();

  return {
    demo: demo !== null,

    previewMetrics() {
      if (!demo) {
        return null;
      }

      return demo.metrics();
    },

    async call<T>(method: string, params?: unknown): Promise<CallResult<T>> {
      if (demo) {
        try {
          return { ok: true, data: await demo.invoke<T>(method, params) };
        } catch (error) {
          return { ok: false, reason: "failed", message: String(error) };
        }
      }

      try {
        return { ok: true, data: await bridge.invoke<T>(method, params) };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return {
          ok: false,
          reason: isMissingMethod(message) ? "unavailable" : "failed",
          message,
        };
      }
    },

    send(type, payload) {
      bridge.send(type, payload);
    },

    on<T>(type: string, handler: (payload: T) => void): () => void {
      return bridge.on<T>(type, handler);
    },
  };
}
