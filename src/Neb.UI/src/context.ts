import type { I18n } from "./i18n";
import type { CallResult, Host } from "./host";
import type { AppConfig, Metrics, ScreenId, ShellState, WindowsState } from "./state";
import type { ToastKind } from "./components/toast";

export interface UpdateVerdict {
  available: boolean;
  version: string | null;
}

export interface AppContext {
  readonly i18n: I18n;
  readonly host: Host;
  /** True when the window is running from `npm run preview` with demo data. */
  readonly demo: boolean;
  config(): AppConfig;
  shell(): ShellState | null;
  /** The last `windows.getState` answer, or null when the host does not implement it. */
  windows(): WindowsState | null;
  /** Re-reads `windows.getState`; resolves with null when the method is unavailable. */
  refreshWindows(): Promise<WindowsState | null>;
  metrics(): Metrics | null;
  version(): string;
  channel(): string;
  navigate(screen: ScreenId): void;
  notify(message: string, kind?: ToastKind): void;
  /** `unavailable` for a method this host build does not have, `failed` for a real error. */
  notifyFailure(result: Extract<CallResult<unknown>, { ok: false }>): void;
  /** Writes one dotted config path and adopts the host's answer. */
  saveConfig(path: string, value: unknown): Promise<boolean>;
  /**
   * Applies partial resets of one or more config sections in one action. Each `values` object is
   * merged on top of its section, so keys the reset does not name keep their value. The shell is
   * reloaded once after all sections were written.
   */
  resetSections(sections: ReadonlyArray<{ section: string; values: Record<string, unknown> }>): Promise<boolean>;
  /** Updates the local mirror only (after a method that already persisted the value itself). */
  setLocalConfig(path: string, value: unknown): void;
  refreshShell(): Promise<void>;
  setThemeName(name: string): void;
  checkUpdates(): Promise<UpdateVerdict | null>;
}

export interface Screen {
  readonly el: HTMLElement;
  activate?(): void | Promise<void>;
  deactivate?(): void;
  render(): void;
}
