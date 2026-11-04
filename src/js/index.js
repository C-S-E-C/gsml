// ── Index (launcher shell) page module ───────────────────────────────
// index.html loads this module. It owns the top app bar (tabs, brand,
// window controls), the logo easter egg and the root-page bootstrap.

import { isRootPage,invokeFn } from './main.js';
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
    document.getElementById("main").addEventListener('contextmenu', event => event.preventDefault());
});

// ── Global bridge ────────────────────────────────────────────────────
// index.js runs as a module, so its top-level declarations are
// module-scoped and NOT visible to inline `onclick="..."` attributes in
// index.html. Expose the handlers used by inline event handlers.
window.sidebar_button = sidebar_button;
window.logoclick = logoclick;