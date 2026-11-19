/**
 * `updates.check` and `updates.whatsNew` have answered in a few shapes while the Rust host was being
 * built (a boolean, a version string, a notes array, a markdown body). These readers normalise them.
 */

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null;
}

export interface UpdateCheck {
  available: boolean;
  version: string | null;
}

export function readUpdateCheck(raw: unknown, current: string): UpdateCheck | null {
  if (isRecord(raw)) {
    if (raw.ok === false || raw.error) {
      return null;
    }

    const version =
      text(raw.version) ??
      text(raw.latest) ??
      text(raw.latestVersion) ??
      text(raw.tag) ??
      text(raw.release) ??
      null;

    const explicit =
      typeof raw.available === "boolean"
        ? raw.available
        : typeof raw.hasUpdate === "boolean"
          ? raw.hasUpdate
          : typeof raw.updateAvailable === "boolean"
            ? raw.updateAvailable
            : null;

    const available = explicit ?? (version !== null && version !== current);
    return { available, version: version ?? null };
  }

  if (typeof raw === "boolean") {
    return { available: raw, version: null };
  }

  if (typeof raw === "string") {
    return { available: raw !== current, version: raw };
  }

  return null;
}

export interface ReleaseNotes {
  notes: string[];
  version: string | null;
}

function toLines(value: unknown): string[] {
  if (typeof value === "string") {
    return value
      .split(/\r?\n/)
      .map((line) => line.replace(/^[-*]\s+/, "").trim())
      .filter((line) => line.length > 0 && !line.startsWith("#"));
  }

  if (Array.isArray(value)) {
    return value.flatMap((entry) => {
      if (typeof entry === "string") {
        return entry.trim() === "" ? [] : [entry.trim()];
      }

      if (isRecord(entry)) {
        const line = text(entry.text) ?? text(entry.title) ?? text(entry.message);
        return line ? [line] : [];
      }

      return [];
    });
  }

  return [];
}

export function readNotes(raw: unknown): ReleaseNotes {
  if (isRecord(raw)) {
    const notes =
      toLines(raw.notes).length > 0
        ? toLines(raw.notes)
        : toLines(raw.changes).length > 0
          ? toLines(raw.changes)
          : toLines(raw.body).length > 0
            ? toLines(raw.body)
            : toLines(raw.markdown);

    return {
      notes,
      version: text(raw.version) ?? text(raw.latest) ?? null,
    };
  }

  return { notes: toLines(raw), version: null };
}
