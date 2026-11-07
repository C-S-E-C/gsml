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
    document.getElementById("device-identifier-art").innerText = drunkenBishop(await getDeviceIdentifier(16), 17,33);
}

/**
 * 生成带边框的 Drunken Bishop 随机艺术图（ASCII Art）
 * 每行内容在右侧边框 | 之前用 '0' 补齐到 width 长度
 *
 * @param {string} input - 十六进制字符串（可包含空格、换行，不区分大小写）
 * @param {number} height - 画布内容高度（行数），需为正整数
 * @param {number} width  - 画布内容宽度（列数），需为正整数
 * @returns {string} 带边框的多行字符串
 */
function drunkenBishop(input, height, width) {
  // ---- 参数校验 ----
  if (typeof input !== 'string') {
    throw new TypeError('input 必须是字符串');
  }
  if (!Number.isInteger(height) || height <= 0) {
    throw new RangeError('height 必须是正整数');
  }
  if (!Number.isInteger(width) || width <= 0) {
    throw new RangeError('width 必须是正整数');
  }

  // ---- 清理并校验十六进制 ----
  const hex = input.replace(/\s+/g, '').toLowerCase();
  if (!/^[0-9a-f]*$/.test(hex)) {
    throw new Error('input 包含非十六进制字符（仅允许 0-9, a-f, A-F, 空白）');
  }

  // ---- 十六进制 -> 字节数组（奇数长度左侧补 0）----
  const paddedHex = hex.length % 2 === 0 ? hex : '0' + hex;
  const bytes = [];
  for (let i = 0; i < paddedHex.length; i += 2) {
    bytes.push(parseInt(paddedHex.substr(i, 2), 16));
  }

  // ---- 初始化画布 ----
  const STEP_CHARS = [' ', '.', '`', ':', ';', '+', '=', 'x', 'X', '$', '&', '#', '%', 'W', '@'];
  const grid = Array.from({ length: height }, () => new Array(width).fill(0));

  // 起始坐标
  const START_X = (width - 1) / 2;
  const START_Y = (height - 1) / 2;

  let x = Math.floor(START_X);
  let y = Math.floor(START_Y);

  if (x < 0) x = 0; else if (x >= width) x = width - 1;
  if (y < 0) y = 0; else if (y >= height) y = height - 1;

  // 起点计入一次脚印
  grid[y][x] += 1;

  // ---- 方向映射 ----
  const DIRS = [
    [-1, -1], // 00 左上
    [ 1, -1], // 01 右上
    [-1,  1], // 10 左下
    [ 1,  1]  // 11 右下
  ];

  // ---- 醉汉游走 ----
  for (let b of bytes) {
    for (let shift = 6; shift >= 0; shift -= 2) {
      const pair = (b >> shift) & 0x03;
      const [dx, dy] = DIRS[pair];
      x += dx;
      y += dy;

      if (x < 0) x = 0;
      else if (x >= width) x = width - 1;
      if (y < 0) y = 0;
      else if (y >= height) y = height - 1;

      grid[y][x] += 1;
    }
  }

  // ---- 渲染为带边框的字符串 ----
  const border =  '═'.repeat(width);
  const lines = [];
  lines.push('╔'+border+'╗');
  const title = 'Drunken Bishop';
  lines.push('║' + '\t'.repeat(Math.floor((width - title.length) / 2)) + title + '\t'.repeat(width - title.length - Math.floor((width - title.length) / 2)) + '║');
  lines.push('╠'+border+'╣');

  for (let row = 0; row < height; row++) {
    let body = '';
    for (let col = 0; col < width; col++) {
      const count = grid[row][col];
      if (count === 0) body += '\t';
      else if (count >= 15) body += '@';
      else body += STEP_CHARS[count];
    }
    // 显式补齐：若 body 不足 width，用 '0' 补足
    // （正常情况下上面的循环已保证长度恰好为 width，此处为防御性补齐）
    if (body.length < width) {
      body += '0'.repeat(width - body.length);
    }
    lines.push('║' + body + '║');
  }

  lines.push('╚'+border+'╝');
  return lines.join('\n');
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
