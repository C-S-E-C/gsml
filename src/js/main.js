// ── Shared core module ────────────────────────────────────────────────
// Public helpers used by every page. Page-specific logic lives in
// per-page modules loaded by each page:
//   · index.js     → launcher shell (tabs, window controls, logo)
//   · home.js      → Home page
//   · versions.js  → Download (installed versions) page
// main.js exposes these helpers via `export` for import by those modules.
// It performs no page bootstrapping of its own.

import { invoke } from '@tauri-apps/api/core';
// Tauri 2 事件监听（withGlobalTauri 注入的 window.__TAURI__）
import { listen } from '@tauri-apps/api/event';

export const invokeFn = invoke;
export const listenFn = listen;

export function isRootPage() {
    return location.pathname === '/' || location.pathname.endsWith('/index.html');
}

export function isPage(name) {
    return location.pathname === `/${name}` || location.pathname.endsWith(`/${name}`);
}

export function escapeHtml(text) {
    return String(text).replace(/[&<>"']/g, (c) => ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;'
    }[c]));
}

