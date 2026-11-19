/**
 * The dashboard side of the WebView2 bridge.
 *
 * Host -> dashboard: `{"type":"neb.metrics","id":null,"ok":true,"payload":{...},"error":null}`
 * Dashboard -> host: `{"type":"neb.invoke","id":"1","method":"config.get","params":{}}`
 *
 * The `payload: { method, params }` spelling is sent as well, so the same bundle talks to a host that
 * reads the older envelope. The same bundle also runs in a plain browser (`npm run preview`), where
 * `chrome.webview` is absent: then `available` is false, `invoke` rejects and `on` never fires, so the
 * UI can show its preview data and never crash.
 */

export interface BridgeMessage {
  type: string;
  id?: string | null;
  ok?: boolean;
  payload?: unknown;
  error?: string | null;
}

/** The subset of `window.chrome.webview` the dashboard uses. */
export interface WebView2Port {
  postMessage(message: unknown): void;
  addEventListener(type: "message", handler: (event: MessageEvent<unknown>) => void): void;
  removeEventListener(type: "message", handler: (event: MessageEvent<unknown>) => void): void;
}

declare global {
  interface Window {
    chrome?: { webview?: WebView2Port };
  }
}

export interface Bridge {
  /** True inside the WPF host, false in a plain browser tab. */
  readonly available: boolean;
  /** Sends a fire-and-forget message to the host. */
  send(type: string, payload?: unknown): void;
  /** Calls a host method and resolves with its payload. */
  invoke<T>(method: string, params?: unknown): Promise<T>;
  /** Subscribes to host messages of `type`; the returned function unsubscribes. */
  on<T>(type: string, handler: (payload: T) => void): () => void;
}

const InvokeTimeoutMs = 10_000;

/**
 * `account.signIn` waits for the browser callback (up to three minutes in the host), so its
 * promise must outlive the ordinary call timeout.
 */
const SignInTimeoutMs = 190_000;

type Pending = {
  resolve: (payload: unknown) => void;
  reject: (error: Error) => void;
  timeout: number;
};

function createBridge(port: WebView2Port | undefined): Bridge {
  const pending = new Map<string, Pending>();
  const handlers = new Map<string, Set<(payload: unknown) => void>>();
  let nextId = 1;

  const settle = (message: BridgeMessage) => {
    if (message.type !== "neb.invoke.result" || typeof message.id !== "string") {
      return false;
    }

    const call = pending.get(message.id);
    if (!call) {
      return true;
    }

    pending.delete(message.id);
    clearTimeout(call.timeout);

    if (message.ok === false) {
      call.reject(new Error(message.error ?? `invoke '${message.id}' failed`));
    } else {
      // Older hosts answered under `result`; the current one uses `payload`.
      call.resolve(message.payload ?? (message as { result?: unknown }).result ?? null);
    }

    return true;
  };

  const dispatch = (message: BridgeMessage) => {
    if (settle(message)) {
      return;
    }

    const listeners = handlers.get(message.type);
    if (!listeners) {
      return;
    }

    for (const listener of listeners) {
      listener(message.payload ?? null);
    }
  };

  const onMessage = (event: MessageEvent<unknown>) => {
    const data = event.data;
    if (typeof data === "string") {
      try {
        dispatch(JSON.parse(data) as BridgeMessage);
      } catch {
        // A host message is always JSON; anything else is not ours to interpret.
      }

      return;
    }

    if (data && typeof data === "object" && typeof (data as BridgeMessage).type === "string") {
      dispatch(data as BridgeMessage);
    }
  };

  if (port) {
    port.addEventListener("message", onMessage);
  }

  return {
    available: Boolean(port),

    send(type, payload) {
      port?.postMessage(payload === undefined ? { type } : { type, payload });
    },

    invoke<T>(method: string, params?: unknown): Promise<T> {
      if (!port) {
        return Promise.reject(new Error(`bridge is unavailable, '${method}' was not sent`));
      }

      const id = String(nextId++);

      return new Promise<T>((resolve, reject) => {
        const timeout = window.setTimeout(() => {
          pending.delete(id);
          reject(new Error(`'${method}' timed out`));
        }, method === "account.signIn" ? SignInTimeoutMs : InvokeTimeoutMs);

        pending.set(id, { resolve: resolve as (payload: unknown) => void, reject, timeout });
        port.postMessage({
          type: "neb.invoke",
          id,
          method,
          params: params ?? {},
          payload: { method, params: params ?? {} },
        });
      });
    },

    on<T>(type: string, handler: (payload: T) => void): () => void {
      const listeners = handlers.get(type) ?? new Set<(payload: unknown) => void>();
      const wrapped = handler as (payload: unknown) => void;
      listeners.add(wrapped);
      handlers.set(type, listeners);

      return () => {
        listeners.delete(wrapped);
        if (listeners.size === 0) {
          handlers.delete(type);
        }
      };
    },
  };
}

/** The dashboard's single bridge instance. */
export const bridge: Bridge = createBridge(
  typeof window === "undefined" ? undefined : window.chrome?.webview,
);
