// ── Index (launcher shell) page module ───────────────────────────────
// index.html loads this module. It owns the top app bar (tabs, brand,
// window controls), the logo easter egg and the root-page bootstrap.

import { isRootPage, invokeFn, listenFn } from './main.js';
import { initAutoUpdate } from './updater.js';
import logoImg from '../assets/logo.png';

const thesquidsays = [
    "Ayo, stop touching me!",
    "I'm not a toy!",
    "Leave me alone!",
    "Don't you understand?",
    "Hell nah!"
];

function sidebar_button(name) {
    const frame = document.getElementById('main').getElementsByTagName('iframe')[0];
    if (frame) {
        frame.src = `/${name}.html`;
    }
}

// ── 事件转发桥 ────────────────────────────────────────────────────────
// Tauri 的 app.emit 只把事件投递到顶层文档，而页面都装在 #main 的 iframe
// 里、自己在 iframe 内 listen 收不到。这里在顶层（常驻文档，不随换 tab
// 重载）统一订阅，再用 postMessage 转发给当前 iframe 页面；页面侧通过
// main.js 的 onPageEvent() 接收。
const RELAYED_EVENTS = ['install-progress', 'ms-login-progress'];

async function setupEventRelay() {
    for (const name of RELAYED_EVENTS) {
        try {
            await listenFn(name, (event) => {
                const frame = document.querySelector('#main iframe');
                frame?.contentWindow?.postMessage(
                    { __gsmlRelay: name, payload: event.payload },
                    location.origin,
                );
            });
        } catch (error) {
            console.error(`Event relay failed for "${name}":`, error);
        }
    }
}

// Window controls (Material 3 custom titlebar)
function setupWindowControls() {
    const minBtn = document.getElementById('min-btn');
    const closeBtn = document.getElementById('close-btn');

    const tauriWindow = window.__TAURI__?.window?.getCurrentWindow?.();

    if (minBtn && tauriWindow) {
        minBtn.addEventListener('click', () => tauriWindow.minimize());
    }
    if (closeBtn && tauriWindow) {
        closeBtn.addEventListener('click', () => tauriWindow.close());
    }
}

var logoclickedtimes = 0;
function logoclick() {
    if (logoclickedtimes == thesquidsays.length) {
        document.getElementById("logo").src = logoImg;
        document.getElementById("js-css").innerHTML += `:root {
           --root-bgcolor: #320000;
           --text-color: #f00;
           --bg-color: #4d0000;
        }`;
        alert("YOU PISSED ME OFF!");
        logoclickedtimes = 0;
    } else {
        alert("The squid:\n" + thesquidsays[logoclickedtimes]);
    }
    logoclickedtimes++;
}

document.addEventListener('DOMContentLoaded', async () => {
    if (!isRootPage()) {
        return;
    }

    // 尽早订阅：登录/安装最开始的进度事件也不会丢。
    void setupEventRelay();

    setupWindowControls();
    // StartTimes now lives in simpleValues (JSON). null = license not agreed yet.
    // NOTE: 0 is a valid value ("agreed, zero launches"), so only null redirects.
    var st = await invokeFn("get_svar", { key: "StartTimes" });
    if (st == null) {
        location.pathname = '/user_license.html';
        return;
    }
    // value is now any JSON type; Number() normalizes legacy string archives.
    await invokeFn("set_svar", { key: "StartTimes", value: (Number(st) || 0) + 1 });
    sidebar_button('home');
    // 启动自动检查更新：fire-and-forget，不阻塞、不强制（发现新版本
    // 只弹可忽略的角标，用户点了才下载应用）
    void initAutoUpdate();
});

// ── Global bridge ────────────────────────────────────────────────────
// index.js runs as a module, so its top-level declarations are
// module-scoped and NOT visible to inline `onclick="..."` attributes in
// index.html. Expose the handlers used by inline event handlers.
window.sidebar_button = sidebar_button;
window.logoclick = logoclick;