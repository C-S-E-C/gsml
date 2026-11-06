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

// ── 事件订阅（iframe 兼容层）──────────────────────────────────────────
// Tauri 2 在 Windows/WebView2 上把 app.emit 的事件投递到 webview 的顶层
// 文档；而 listen() 的回调 id 注册在发起调用的文档（本应用里是 #main 的
// iframe）里。顶层文档查不到 iframe 注册的 id，每个事件都会在控制台报
// "Couldn't find callback id"，且 iframe 内进度条永远收不到更新。
// 统一走 onPageEvent：
//   · 页面本身是顶层文档 → 直接 listen；
//   · 页面在 iframe 内   → 由顶层壳 index.js 订阅并 postMessage 转发，
//     页面监听 window 'message'。返回清理函数（Promise<() => void>）。
export async function onPageEvent(name, handler) {
    if (window.self === window.top) {
        return listenFn(name, (event) => handler(event.payload));
    }
    const relay = (e) => {
        const data = e.data;
        if (data && data.__gsmlRelay === name) {
            handler(data.payload);
        }
    };
    window.addEventListener('message', relay);
    return () => window.removeEventListener('message', relay);
}

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

// document.addEventListener('contextmenu', event => event.preventDefault());
