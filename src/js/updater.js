// ── Auto-update flow (GitHub release) ─────────────────────────────────
// Owns the whole update pipeline for the settings "Update" card:
//   1. check    → invoke check_update (server choice + is_debug 渠道在
//                 Rust 侧决定：debug 构建跟随 prerelease)
//   2. download → invoke download_update, progress via 'update-progress'
//   3. apply    → invoke apply_update (taskkill/rm/mv 自替换或拉起安装器)

import { invokeFn, onPageEvent, isRootPage } from './main.js';

// 与 settings.js 的 SVAR 常量保持一致（跨模块不互相 import，避免页面
// 逻辑依赖）。简单值都存后端 simpleValues（get_svar/set_svar）。
const PREF = {
    enabled: 'auto-update-enabled',
    server: 'auto-update-server',
    customUrl: 'custom-update-server-url',
};

// 最近一次 check_update 的结果，供 apply 前复用 asset 信息
let lastCheck = null;

/**
 * 检查更新。
 * @param {string|null} serverBase 自定义 release 源（空/null → 官方仓库）
 * @returns {Promise<object>} UpdateCheck（含 up_to_date / asset_url / mode）
 */
export async function checkForUpdates(serverBase = null) {
    lastCheck = await invokeFn('check_update', { serverBase: serverBase || null });
    return lastCheck;
}

/** 订阅后端下载进度事件，返回取消订阅函数 */
export function onUpdateProgress(handler) {
    return onPageEvent('update-progress', (payload) => handler(payload));
}

/** 下载上一次 check 选中的 asset 到临时目录，返回落盘路径 */
export async function downloadUpdate(onProgress = () => {}) {
    if (!lastCheck || !lastCheck.asset_url) {
        throw new Error('请先检查更新');
    }
    const unlisten = await onUpdateProgress(onProgress);
    try {
        return await invokeFn('download_update', {
            url: lastCheck.asset_url,
            name: lastCheck.asset_name,
        });
    } finally {
        unlisten?.();
    }
}

/** 应用已下载的更新（分离进程替换/拉起安装器），成功后本进程退出 */
export async function applyDownloaded(file) {
    await invokeFn('apply_update', { file });
}

/**
 * 下载并立即应用（不中断确认，适合用户已明确点击「立即更新」的场景）。
 * @returns {Promise<void>} 成功后后端会自行退出本进程（或拉起安装器）
 */
export async function downloadAndApply(onProgress = () => {}) {
    const file = await downloadUpdate(onProgress);
    await applyDownloaded(file);
}

// ── 启动时自动检查（非阻塞、不强制）──────────────────────────────────
// 由 shell（index.js）在 DOMContentLoaded 后 fire-and-forget 调用：
//   · 仅在根页面执行一次，避免换 tab 重载 iframe 时重复检查；
//   · async invoke 不占主进程；整条链路任何失败都静默吞掉；
//   · 发现新版本只弹一个可忽略的角标通知，用户点「立即更新」才走
//     下载/应用，绝不自动静默替换。

/** 读 simpleValues 偏好；失败按默认值处理 */
async function pref(key, fallback) {
    try {
        const value = await invokeFn('get_svar', { key });
        return value ?? fallback;
    } catch {
        return fallback;
    }
}

/** 更新角标：右下角非模态通知条，可关闭，可继续下载 */
function showUpdateToast({ version, isPrerelease, onUpdate, onDismiss }) {
    const toast = document.createElement('div');
    toast.id = 'gsml-update-toast';
    Object.assign(toast.style, {
        position: 'fixed', right: '16px', height: '16px', zIndex: '9999', top: '64px',
        display: 'flex', alignItems: 'center', gap: '12px',
        padding: '12px 16px', borderRadius: '5px',
        background: 'var(--md-sys-color-surface-container-high, #2a2a33)',
        color: 'var(--md-sys-color-on-surface, #e6e0e9)',
        boxShadow: '0 4px 16px rgba(0,0,0,.35)', fontSize: '14px',
    });
    toast.innerHTML = `
        <span>Found new version: ${version}${isPrerelease ? ' (pre-release)' : ''}</span>
        <button id="gsml-update-now" style="border:0;padding:6px 14px;border-radius:999px;
            background:var(--md-sys-color-primary,#006a60);color:var(--md-sys-color-on-primary,#fff);
            cursor:pointer;">Update</button>
        <button id="gsml-update-skip" style="border:0;padding:6px 10px;border-radius:999px;
            background:transparent;color:inherit;cursor:pointer;">Ignore</button>`;
    document.body.appendChild(toast);
    const close = () => toast.remove();
    toast.querySelector('#gsml-update-skip').addEventListener('click', () => {
        close();
        onDismiss?.();
    });
    toast.querySelector('#gsml-update-now').addEventListener('click', () => {
        const text = toast.querySelector('span');
        const nowBtn = toast.querySelector('#gsml-update-now');
        const skipBtn = toast.querySelector('#gsml-update-skip');
        nowBtn.disabled = skipBtn.disabled = true;
        nowBtn.style.opacity = '.6';
        onUpdate(text, close);
    });
}

/**
 * 启动自动检查入口。失败/已是最新一律静默；发现新版本时提示但不强制。
 * 调用方（index.js）用 `void initAutoUpdate()` fire-and-forget。
 */
export async function initAutoUpdate() {
    if (!isRootPage() || window.__gsmlAutoUpdateRan) return;
    window.__gsmlAutoUpdateRan = true;

    try {
        // 稍等 3 秒再查，把启动窗口让给首屏渲染
        await new Promise((r) => setTimeout(r, 3000));
        if ((await pref(PREF.enabled, true)) === false) return;

        const server = (await pref(PREF.server, 'github')) || 'github';
        const serverBase = server === 'custom' ? await pref(PREF.customUrl, '') : '';
        const info = await checkForUpdates(serverBase || null);
        if (info.up_to_date) return;

        showUpdateToast({
            version: info.latest_version,
            isPrerelease: info.is_prerelease,
            onUpdate: async (text, close) => {
                try {
                    await downloadAndApply(({ label, done, total }) => {
                        const pct = total > 0 ? Math.round((done * 100) / total) : 0;
                        text.textContent = label || '下载中'+pct+'%';
                    });
                    text.textContent = '更新完成，即将重启…';
                } catch (err) {
                    console.error('auto update failed:', err);
                    text.textContent = `更新失败：${err}`;
                    // 失败不关闭，允许用户看一眼再手动忽略
                    const skip = document.getElementById('gsml-update-skip');
                    if (skip) { skip.disabled = false; skip.textContent = '关闭'; }
                }
            },
        });
    } catch (err) {
        // 自动检查是纯可选功能，任何错误都只留日志
        console.error('auto update check failed:', err);
    }
}
