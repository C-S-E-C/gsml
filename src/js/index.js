// ── Index (launcher shell) page module ───────────────────────────────
// index.html loads this module. It owns the top app bar (tabs, brand,
// window controls), the logo easter egg and the root-page bootstrap.

import { isRootPage, invokeFn, listenFn, escapeHtml } from './main.js';
import { initAutoUpdate } from './updater.js';
import logoImg from '../assets/logo.png';

const THE_SQUID_SAYS = [
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
const RELAYED_EVENTS = [
    'install-progress',
    'ms-login-progress',
    'launch-status',
    'game-output',
    // 设置页的「Check for Updates」进度条：Rust 发 update-progress，
    // 漏了这条转发会让 iframe 里的进度条永远停在 0。
    'update-progress',
    // 自动安装 Java 的进度（设置页 Java 面板 + 下载页安装流程）
    'java-install-progress',
];

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

// ── Deeplink 动作（install-version / start-game）──────────────────────
// 浏览器打开 glow-squid://install-version?... 会唤起启动器；Rust 解析后发
// deeplink-action，这里先弹确认框，确认后切到对应页面并把动作转交 iframe。
const LOADER_LABEL = ['Vanilla', 'Fabric', 'Forge', 'NeoForge'];

/** 按分隔符切 token 后整体比较（1.21.1 不会命中 1.21.11）。 */
function versionTokenMatch(text, wanted) {
    if (!text) {
        return false;
    }
    return text.toLowerCase().split(/[^0-9a-z.]+/).includes(wanted.toLowerCase());
}

/**
 * 本地实例是否属于某个原版版本：名字里带该版本号，或 inheritsFrom 就是它。
 * 这样 `1.21.11-Fabric 0.18.3`、`fabric-loader-0.18.3-1.21.11`、
 * `1.21.11-FABRIC-OPTIMISED` 都能被 `1.21.11` 找到。
 */
function matchesGameVersion(ver, wanted) {
    const target = wanted.trim();
    if (!target) {
        return true;
    }
    if (ver.name === target || ver.official_name === target) {
        return true;
    }
    return versionTokenMatch(ver.official_name, target) || versionTokenMatch(ver.name, target);
}

function launchText(ver, server) {
    const loader = LOADER_LABEL[ver.loader] || 'Vanilla';
    const serverText = server ? `\nAnd auto-join server: ${server}` : '';
    return `Minecraft ${ver.name} (${loader})${serverText}\n\nContinue to go back to Home and launch it.`;
}

/**
 * 通用确认框：`choices` 非空时渲染可选列表（点某行即选中并确认，
 * 此时隐藏 Continue 按钮）。返回 { ok, choice: 行下标 | null }。
 */
function showDeeplinkDialog({ title, text, choices }) {
    return new Promise((resolve) => {
        const dialog = document.getElementById('deeplink-dialog');
        const titleEl = document.getElementById('deeplink-dialog-title');
        const textEl = document.getElementById('deeplink-dialog-text');
        const listEl = document.getElementById('deeplink-dialog-choices');
        const okBtn = document.getElementById('deeplink-dialog-confirm');
        const cancelBtn = document.getElementById('deeplink-dialog-cancel');
        if (!dialog || !titleEl || !textEl || !listEl || !okBtn || !cancelBtn) {
            resolve({ ok: false, choice: null });
            return;
        }
        const hasChoices = Array.isArray(choices) && choices.length > 0;
        titleEl.textContent = title || '';
        textEl.textContent = text || '';
        textEl.hidden = !text;
        listEl.hidden = !hasChoices;
        listEl.innerHTML = hasChoices
            ? choices.map((choice, index) => `
                <md-list-item type="button" data-choice="${index}">
                    <md-icon slot="start">deployed_code</md-icon>
                    <div slot="headline">${escapeHtml(choice.headline)}</div>
                    <div slot="supporting-text">${escapeHtml(choice.supporting || '')}</div>
                </md-list-item>`).join('')
            : '';
        okBtn.hidden = hasChoices; // 有列表时点行即选

        const finish = (value) => {
            okBtn.removeEventListener('click', onOk);
            cancelBtn.removeEventListener('click', onCancel);
            listEl.removeEventListener('click', onPick);
            dialog.removeEventListener('close', onClose);
            if (dialog.open) {
                void dialog.close();
            }
            resolve(value);
        };
        const onOk = () => finish({ ok: true, choice: null });
        const onCancel = () => finish({ ok: false, choice: null });
        const onPick = (event) => {
            const row = event.target.closest('[data-choice]');
            if (!row) {
                return;
            }
            finish({ ok: true, choice: Number(row.dataset.choice) });
        };
        const onClose = () => finish({ ok: false, choice: null });
        okBtn.addEventListener('click', onOk);
        cancelBtn.addEventListener('click', onCancel);
        listEl.addEventListener('click', onPick);
        dialog.addEventListener('close', onClose);
        void dialog.show();
    });
}

/** install-version：切到 Download 页并预填详情（不自动安装）。 */
async function handleInstallAction(action) {
    const loader = Number(action.loader) || 0;
    const lv = action.loaderVersion ? ` ${action.loaderVersion}` : '';
    const name = action.name ? `\nInstance name: ${action.name}` : '';
    const res = await showDeeplinkDialog({
        title: 'Go to install?',
        text: `Minecraft ${action.version} (${LOADER_LABEL[loader] || 'Vanilla'}${lv})${name}`
            + '\n\nContinue to open the Download page with these options prefilled.'
            + ' You can change the loader / version there and click Install when ready.',
    });
    if (res.ok) {
        navigateAndPost('download', action);
    }
}

/**
 * start-game：`version` 可选。
 *   · 给了版本号 → 找出所有属于该版本的本地实例（名称/loader 不限）；
 *     只有一个就直接确认，多个就列出来让用户选；
 *   · 没给 → 列出全部本地实例让用户选。
 */
async function handleLaunchAction(action) {
    const wanted = String(action.version || '').trim();
    let local = [];
    try {
        local = await invokeFn('get_local_versions');
    } catch (error) {
        console.error('get_local_versions failed:', error);
    }
    if (!Array.isArray(local)) {
        local = [];
    }
    const matches = wanted ? local.filter((ver) => matchesGameVersion(ver, wanted)) : local;
    if (!matches.length) {
        await showDeeplinkDialog({
            title: 'Cannot launch',
            text: wanted
                ? `No local instance for ${wanted}. Install it on the Download page first.`
                : 'No local version is installed yet.',
        });
        return;
    }
    if (matches.length === 1) {
        const res = await showDeeplinkDialog({
            title: 'Launch game?',
            text: launchText(matches[0], action.server),
        });
        if (res.ok) {
            navigateAndPost('home', {
                kind: 'launch',
                version: matches[0].name,
                server: action.server,
            });
        }
        return;
    }
    const res = await showDeeplinkDialog({
        title: wanted ? `Choose an instance of ${wanted}` : 'Choose an instance to launch',
        text: `${matches.length} instances found; click one to launch:`,
        choices: matches.map((ver) => ({
            headline: ver.name,
            supporting: `${LOADER_LABEL[ver.loader] || 'Vanilla'}${
                ver.official_name && ver.official_name !== ver.name ? ` · ${ver.official_name}` : ''
            }`,
        })),
    });
    if (!res.ok || res.choice == null) {
        return;
    }
    const picked = matches[res.choice];
    navigateAndPost('home', {
        kind: 'launch',
        version: picked.name,
        server: action.server,
    });
}

/** 切到 page 并把动作 postMessage 给 iframe（页面已加载则直接发）。 */
function navigateAndPost(page, action) {
    const frame = document.querySelector('#main iframe');
    if (!frame) {
        return;
    }
    const target = `/${page}.html`;
    const send = () => {
        frame.contentWindow?.postMessage({ __gsmlAction: action }, location.origin);
    };
    if (frame.getAttribute('src') === target) {
        send();
        return;
    }
    frame.addEventListener('load', () => send(), { once: true });
    frame.src = target;
}

async function setupDeeplinkActions() {
    /** 弹确认框并路由到对应页面。 */
    const handle = async (action) => {
        if (!action || !action.kind) {
            return;
        }
        if (action.kind === 'install') {
            await handleInstallAction(action);
        } else if (action.kind === 'launch') {
            await handleLaunchAction(action);
        }
    };

    try {
        await listenFn('deeplink-action', (event) => {
            void handle(event.payload);
        });
        // 冷启动（浏览器唤起启动器）时动作可能在页面就绪前就到了，这里补领
        const pending = await invokeFn('deeplink_ready').catch(() => []);
        for (const action of Array.isArray(pending) ? pending : []) {
            await handle(action);
        }
    } catch (error) {
        console.error('deeplink listener failed:', error);
    }
}

var logoClickedTimes = 0;
function logoclick() {
    if (logoClickedTimes == THE_SQUID_SAYS.length) {
        document.getElementById("logo").src = logoImg;
        document.getElementById("js-css").innerHTML += `:root {
           --root-bgcolor: #320000;
           --text-color: #f00;
           --bg-color: #4d0000;
        }`;
        alert("YOU PISSED ME OFF!");
        logoClickedTimes = 0;
    } else {
        alert("The squid:\n" + THE_SQUID_SAYS[logoClickedTimes]);
    }
    logoClickedTimes++;
}

document.addEventListener('DOMContentLoaded', async () => {
    if (!isRootPage()) {
        return;
    }

    // 尽早订阅：登录/安装最开始的进度事件也不会丢。
    void setupEventRelay();
    // 浏览器唤起启动器的动作（install-version / start-game）
    void setupDeeplinkActions();

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