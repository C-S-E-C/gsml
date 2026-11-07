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
import * as app from '@tauri-apps/api/app';

export const invokeFn = invoke;
export const listenFn = listen;
export const tauriapp = app;

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
// ── Device identifier 缓存 ─────────────────────────────────────────────
// 之前这里是模块顶层 await：main.js 被所有页面导入，顶层 await 会把整个
// 模块图（home 账号加载、各页面初始化）都阻塞在这一次 invoke 上，invoke
// 慢或失败时页面直接卡死。现在改为「后台预热 + 异步获取助手」：
//   · sessionStorage 已有缓存 → 直接用，不发起 invoke；
//   · 无缓存 → 立即在后台取一次（不阻塞模块求值），取到后写回缓存；
//   · 失败时清空 in-flight 记录，下次调用可重试；
//   · 并发调用共享同一个 Promise，不会重复 invoke。
let deviceIdentifierPromise = null;

export function getDeviceIdentifier() {
    const cached = sessionStorage.getItem('device-identifier');
    if (cached) return Promise.resolve(cached);
    deviceIdentifierPromise ??= invokeFn('get_device_identifier')
        .then((id) => {
            // 空结果不写缓存（下次调用会重试），避免缓存住异常返回值。
            if (id) sessionStorage.setItem('device-identifier', id);
            return id;
        })
        .catch((err) => {
            // 失败不缓存、不留死 Promise，允许后续重试。
            deviceIdentifierPromise = null;
            console.error('get_device_identifier failed:', err);
            return null;
        });
    return deviceIdentifierPromise;
}

// 后台预热：尽早填充缓存，但绝不阻塞任何页面的加载。
getDeviceIdentifier();