// ── Auto-update flow (GitHub release) ─────────────────────────────────
// Owns the whole update pipeline for the settings "Update" card:
//   1. check    → invoke check_update (server choice + is_debug 渠道在
//                 Rust 侧决定：debug 构建跟随 prerelease)
//   2. download → invoke download_update, progress via 'update-progress'
//   3. apply    → invoke apply_update (taskkill/rm/mv 自替换或拉起安装器)

import { invokeFn, onPageEvent } from './main.js';

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

/**
 * 下载上一次 check 选中的 asset 并应用更新。
 * @param {(payload: {stage:string,done:number,total:number,label:string}) => void} onProgress
 * @returns {Promise<void>} 成功后后端会自行退出本进程（或拉起安装器）
 */
export async function downloadAndApply(onProgress = () => {}) {
    if (!lastCheck || !lastCheck.asset_url) {
        throw new Error('请先检查更新');
    }
    const unlisten = await onUpdateProgress(onProgress);
    try {
        const file = await invokeFn('download_update', {
            url: lastCheck.asset_url,
            name: lastCheck.asset_name,
        });
        await invokeFn('apply_update', { file });
    } finally {
        unlisten?.();
    }
}
