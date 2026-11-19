/**
 * Inline SVG icons. They are part of the bundle on purpose: the dashboard must not fetch anything at
 * runtime, and the WebView2 window has no network access to fall back on.
 */

const stroke = 'fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"';

const paths = {
  home: '<path d="M4 10.5 12 4l8 6.5V20H4z"/><path d="M9.5 20v-5.5h5V20"/>',
  customize:
    '<path d="M12 3a9 9 0 1 0 0 18c1.4 0 2.2-.9 2.2-2 0-.9-.6-1.4-.6-2.2 0-1 .8-1.8 1.9-1.8H18a3 3 0 0 0 3-3c0-4.9-4-9-9-9Z"/><circle cx="7.6" cy="11.4" r="1.1"/><circle cx="10.4" cy="7.8" r="1.1"/><circle cx="15" cy="8.2" r="1.1"/>',
  wallpapers:
    '<rect x="3" y="5" width="18" height="14" rx="3"/><circle cx="8.8" cy="10.4" r="1.5"/><path d="m5 17 4.4-4.4 2.9 2.9L15.8 12l3.2 3.4"/>',
  network:
    '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17"/><path d="M12 3.5c2.2 2.4 3.3 5.4 3.3 8.5s-1.1 6.1-3.3 8.5c-2.2-2.4-3.3-5.4-3.3-8.5S9.8 5.9 12 3.5Z"/>',
  discord:
    '<path d="M20 15a3 3 0 0 1-3 3H8.5L4.5 21V6a3 3 0 0 1 3-3h9.5a3 3 0 0 1 3 3z"/><circle cx="9.4" cy="10.6" r="1.1"/><circle cx="14.6" cy="10.6" r="1.1"/>',
  extensions:
    '<path d="M10 4.5a2 2 0 1 1 4 0V6h3a1 1 0 0 1 1 1v3h1.5a2 2 0 1 1 0 4H18v3a1 1 0 0 1-1 1h-3v-1.5a2 2 0 1 0-4 0V18H7a1 1 0 0 1-1-1v-3H4.5a2 2 0 1 1 0-4H6V7a1 1 0 0 1 1-1h3z"/>',
  profiles:
    '<circle cx="9.2" cy="8.6" r="3"/><path d="M3.6 19.4c.6-3 2.8-4.6 5.6-4.6s5 1.6 5.6 4.6"/><path d="M16.2 6.4a2.8 2.8 0 0 1 0 5.2M17.8 15.4c2 .6 3 2 3.3 4"/>',
  settings:
    '<path d="M4 7.5h6M16 7.5h4M4 16.5h4M14 16.5h6"/><circle cx="13" cy="7.5" r="2.2"/><circle cx="11" cy="16.5" r="2.2"/>',
  chevronDown: '<path d="m7 10 5 5 5-5"/>',
  chevronRight: '<path d="m10 7 5 5-5 5"/>',
  play: '<path d="M8.5 5.6v12.8L19 12z" fill="currentColor" stroke="none"/>',
  minimize: '<path d="M4 12h16"/>',
  maximize: '<rect x="5.5" y="5.5" width="13" height="13" rx="1.8"/>',
  restore:
    '<rect x="8.2" y="4.4" width="11.4" height="11.4" rx="1.8"/><path d="M15.8 16.6v2.2a1 1 0 0 1-1 1H5.6a1 1 0 0 1-1-1v-8.4a1 1 0 0 1 1-1h2.4"/>',
  close: '<path d="M6 6 18 18M18 6 6 18"/>',
  sparkles:
    '<path d="M11.5 4 13 8.2l4.2 1.5-4.2 1.5-1.5 4.2-1.5-4.2L5.7 9.7 9.9 8.2z"/><path d="m18 14.6.9 2.4 2.4.9-2.4.9-.9 2.4-.9-2.4-2.4-.9 2.4-.9z"/>',
  refresh: '<path d="M20 11.4a8 8 0 1 0-2.4 6"/><path d="M20.4 4.6v6.6h-6.6"/>',
  monitor:
    '<rect x="3" y="4.5" width="18" height="12" rx="2.5"/><path d="M9 20h6M12 16.5V20"/>',
  zap: '<path d="M13.2 3 5.5 13.6H11l-1.2 7.4 8.2-11.2H12.4z"/>',
  bar: '<rect x="3" y="4.5" width="18" height="4" rx="1.6"/><path d="M6.4 6.5h3.2M14.4 6.5h3.2"/>',
  dock: '<rect x="3" y="15.5" width="18" height="4" rx="1.6"/><path d="M6.4 17.5h3.2M14.4 17.5h3.2"/>',
  launcher: '<circle cx="10.8" cy="10.8" r="6.2"/><path d="m15.4 15.4 4.2 4.2"/>',
  taskbar: '<rect x="3" y="4.5" width="18" height="15" rx="2.4"/><path d="M3 15.5h18"/>',
  folder: '<path d="M3.5 7.5a2 2 0 0 1 2-2h3.3l1.8 2.2h7.9a2 2 0 0 1 2 2v7.8a2 2 0 0 1-2 2H5.5a2 2 0 0 1-2-2z"/>',
  palette:
    '<circle cx="12" cy="12" r="8.2"/><circle cx="9.2" cy="9.6" r="0.9"/><circle cx="14.8" cy="9.6" r="0.9"/><circle cx="9.8" cy="14.8" r="0.9"/>',
  clock: '<circle cx="12" cy="12" r="8.2"/><path d="M12 7.6V12l3.1 2"/>',
  pause: '<rect x="7" y="5.6" width="3.6" height="12.8" rx="1.1"/><rect x="13.4" y="5.6" width="3.6" height="12.8" rx="1.1"/>',
  info: '<circle cx="12" cy="12" r="8.4"/><path d="M12 11v5.2"/><path d="M12 7.8h.01"/>',
  terminal: '<rect x="3" y="4.5" width="18" height="15" rx="2.6"/><path d="m7.4 9.4 2.7 2.6-2.7 2.6"/><path d="M12.6 14.8h3.9"/>',
  download: '<path d="M12 4.4v10"/><path d="m7.8 10.6 4.2 4.2 4.2-4.2"/><path d="M4.6 18.4h14.8"/>',
  check: '<path d="m5.5 12.4 4.2 4.2 8.8-9.2"/>',
  grip: '<circle cx="9" cy="7" r="1"/><circle cx="15" cy="7" r="1"/><circle cx="9" cy="12" r="1"/><circle cx="15" cy="12" r="1"/><circle cx="9" cy="17" r="1"/><circle cx="15" cy="17" r="1"/>',
  plus: '<path d="M12 5.5v13M5.5 12h13"/>',
  trash: '<path d="M4.8 7.2h14.4"/><path d="M9.4 7.2V5.4a1 1 0 0 1 1-1h3.2a1 1 0 0 1 1 1v1.8"/><path d="M6.6 7.2l.8 11.2a1.4 1.4 0 0 0 1.4 1.3h6.4a1.4 1.4 0 0 0 1.4-1.3l.8-11.2"/><path d="M10.2 11v5M13.8 11v5"/>',
  arrowUp: '<path d="M12 19V5"/><path d="m6.8 10.2 5.2-5.2 5.2 5.2"/>',
  arrowDown: '<path d="M12 5v14"/><path d="m6.8 13.8 5.2 5.2 5.2-5.2"/>',
  external: '<path d="M13.6 4.6h5.8v5.8"/><path d="m19.4 4.6-8.2 8.2"/><path d="M17.2 13.4v4.6a1.4 1.4 0 0 1-1.4 1.4H5.4A1.4 1.4 0 0 1 4 18V7.6a1.4 1.4 0 0 1 1.4-1.4h4.6"/>',
  power: '<path d="M12 4.6v6.6"/><path d="M7.6 7.2a7 7 0 1 0 8.8 0"/>',
  tag: '<path d="M4.4 11.3V5.6a1.2 1.2 0 0 1 1.2-1.2h5.7l8.3 8.3a1.6 1.6 0 0 1 0 2.3l-4.6 4.6a1.6 1.6 0 0 1-2.3 0z"/><circle cx="8.4" cy="8.4" r="1.2"/>',
  cloud: '<path d="M7.4 18.2a3.8 3.8 0 0 1-.4-7.6 5.2 5.2 0 0 1 10-1.4 3.5 3.5 0 0 1-.6 9z"/>',
  shield: '<path d="M12 3.8 5.4 6.4v5.1c0 4 2.8 7.3 6.6 8.7 3.8-1.4 6.6-4.7 6.6-8.7V6.4z"/><path d="m9.2 12.2 2 2 3.6-4"/>',
  chevronUp: '<path d="m7 14 5-5 5 5"/>',
} as const;

export type IconName = keyof typeof paths;

/** An inline SVG for `name`, sized `size` px (24 px design grid). */
export function icon(name: IconName, size = 18): string {
  const body = paths[name];
  return `<svg class="icon" width="${size}" height="${size}" viewBox="0 0 24 24" ${stroke} aria-hidden="true" focusable="false">${body}</svg>`;
}
