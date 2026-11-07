// ── Settings page module ───────────────────────────────────────────────
// settings.html loads this module. It owns the settings sidebar (category
// list on the left) and the content pane on the right. Only the "About"
// view exists for now; more views can be added to the list / #view-* panels.

import { isPage,invokeFn,tauriapp,getDeviceIdentifier } from './main.js';
import { checkForUpdates, downloadUpdate, applyDownloaded, onUpdateProgress } from './updater.js';

const VIEWS = [
    { key: 'about' },
    { key: 'update' },
    // Add future settings categories here, e.g. { key: 'general' }.
];

function viewSelector(key) {
    return `#view-${key}`;
}

function navItemSelector(key) {
    return `.settings-nav [data-view="${key}"]`;
}

function setActiveView(key) {
    const panel = document.querySelector(viewSelector(key));
    const navItem = document.querySelector(navItemSelector(key));
    if (!panel) return;

    // Toggle visibility of all registered view panels.
    VIEWS.forEach(({ key: k }) => {
        const p = document.querySelector(viewSelector(k));
        if (p) p.hidden = k !== key;
    });

    // Mark the active nav item as selected.
    document.querySelectorAll('.settings-nav [data-view]').forEach((item) => {
        item.classList.toggle('selected', item.dataset.view === key);
    });
    if (navItem) navItem.classList.add('selected');
}

function switchSettingsView(key) {
    setActiveView(key);
}

async function copy(text) {
    await navigator.clipboard.writeText(text);
}

// ── Update 设置区（view-update）───────────────────────────────────────
// 偏好持久化走后端 simpleValues（get_svar/set_svar）；更新管线在 updater.js。
const SVAR = {
    enabled: 'auto-update-enabled',
    server: 'auto-update-server',
    customUrl: 'custom-update-server-url',
};

async function loadSvar(key, fallback) {
    try {
        const value = await invokeFn('get_svar', { key });
        return value ?? fallback;
    } catch (err) {
        console.error(`get_svar(${key}) failed:`, err);
        return fallback;
    }
}

const saveSvar = (key, value) => invokeFn('set_svar', { key, value: value }).catch(
    (err) => console.error(`set_svar(${key}) failed:`, err),
);

function updateStatus(text, { showProgress = false, progress = 0 } = {}) {
    const status = document.getElementById('update-status');
    const bar = document.getElementById('update-progress');
    if (status) {
        status.textContent = text;
        status.hidden = !text;
    }
    if (bar) {
        bar.hidden = !showProgress;
        bar.value = progress;
    }
}

/** M3 更新确认对话框：resolve(true) = 用户选择立即更新 */
function showUpdateConfirmDialog(text) {
    return new Promise((resolve) => {
        const dialog = document.getElementById('update-confirm-dialog');
        const para = document.getElementById('update-confirm-text');
        if (!dialog || !para) { resolve(window.confirm(text)); return; }
        // line breaks in dialog
        para.textContent = text;
        dialog.addEventListener('close', () => resolve(dialog.returnValue === 'update'), { once: true });
        dialog.show();
    });
}

async function runCheck() {
    const button = document.getElementById('check-update-button');
    const customUrl = document.getElementById('custom-update-server-url')?.value?.trim() || null;
    try {
        button.disabled = true;
        updateStatus('正在检查更新…');
        const info = await checkForUpdates(customUrl);
        if (info.up_to_date) {
            updateStatus(`已是最新版本 ${info.latest_version}`);
            return;
        }
        const channel = info.is_prerelease ? '（预发布）' : '';
        // 不强更：发现新版本先用 M3 对话框征求同意，拒绝则停在提示页
        const confirmed = await showUpdateConfirmDialog(
            `发现新版本 ${info.latest_version}${channel}（${info.asset_name}）。` +
            '\n要下载并安装吗？安装会重启启动器。',
        );
        if (!confirmed) {
            updateStatus(`发现新版本 ${info.latest_version}${channel}，已取消。可稍后再检查。`);
            return;
        }
        updateStatus(`正在下载 ${info.asset_name}…`, {
            showProgress: true,
            progress: 0,
        });
        const file = await downloadUpdate(({ done, total, label }) => {
            updateStatus(label || '下载中…', {
                showProgress: true,
                progress: total > 0 ? done / total : 0,
            });
        });
        await applyDownloaded(file);
    } catch (err) {
        console.error('update failed:', err);
        updateStatus(`更新失败：${err}`);
    } finally {
        button.disabled = false;
    }
}

async function initUpdateSettings() {
    const toggle = document.getElementById('auto-update-toggle');
    const select = document.getElementById('auto-update-fetch-server');
    const customLine = document.getElementById('custom-update-server-line');
    const customUrl = document.getElementById('custom-update-server-url');
    const button = document.getElementById('check-update-button');
    const versionEl = document.getElementById('update-current-version');
    if (!toggle || !select) return;

    // 恢复持久化偏好
    toggle.selected = (await loadSvar(SVAR.enabled, true)) !== false;
    select.value = (await loadSvar(SVAR.server, 'github')) || 'github';
    customUrl.value = await loadSvar(SVAR.customUrl, '') || '';
    customLine.style.display = select.value === 'custom' ? '' : 'none';
    if (versionEl) versionEl.textContent = await tauriapp.getVersion();

    toggle.addEventListener('change', () => saveSvar(SVAR.enabled, toggle.selected));
    select.addEventListener('change', () => {
        customLine.style.display = select.value === 'custom' ? '' : 'none';
        saveSvar(SVAR.server, select.value);
    });
    customUrl.addEventListener('change', () => saveSvar(SVAR.customUrl, customUrl.value.trim()));
    button.addEventListener('click', runCheck);
    // 后台进度（auto-update 轮询等场景未来复用）
    onUpdateProgress(() => {});
}

async function initSettingsPage() {
    // Default to the first (only) view: "about".
    const first = VIEWS.length > 0 ? VIEWS[0].key : 'about';
    setActiveView(first);
    document.getElementsByClassName("setting-version")[0].innerHTML += ` ${await tauriapp.getVersion()} &nbsp;`;
    if (await invokeFn('is_debug')) {
        document.getElementsByClassName("setting-version")[0].innerHTML += `<span class="muted">[PREVIEW]</span>`;
    }
    document.getElementById("device-identifier").innerText = await getDeviceIdentifier();
    document.getElementById("device-identifier").addEventListener("click", async () => {
        await copy((await getDeviceIdentifier()).replaceAll(" ", ""));
    });
}

document.addEventListener('DOMContentLoaded', async () => {
    if (!isPage('settings.html')) {
        return;
    }
    initSettingsPage();
    initUpdateSettings();
});

// ── Global bridge ────────────────────────────────────────────────────
// settings.js runs as a module, so its top-level declarations are not
// visible to inline `onclick="..."` attributes in settings.html.
window.switchSettingsView = switchSettingsView;
