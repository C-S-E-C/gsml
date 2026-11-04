// ── Download page module ─────────────────────────────────────────────
// download.html (root) loads this module. Two-step download flow:
//   1. Remote version list (list_remote_versions loader=0), searchable and
//      filterable by type, all rendered at once; click a card to open
//      the detail view.
//   2. Detail view: pick a loader (vanilla/fabric/forge/neoforge), pick the
//      loader build, optionally rename the instance, install via
//      install_version with progress from the install-progress event.
// Installed versions (get_local_versions) are listed at the bottom; they
// are selectable on the Home page.
//
// NOTE: no MC-related localStorage keys exist here (project convention:
// MC keys are deleted, never migrated to simpleValues).

import {
    isPage,
    escapeHtml,
    invokeFn,
    onPageEvent,
} from './main.js';

// Icons are bundled through Vite; imported so the build hashes them.
import fabricImg from '../assets/fabric.png';
import neoforgeImg from '../assets/neoforge.png';
import anvilImg from '../assets/anvil.png';
import grassBlockImg from '../assets/grass_block.png';
import dirtImg from '../assets/dirt.png';
import cobblestoneImg from '../assets/cobblestone.png';
import musicDiscImg from '../assets/music_disc_11.png';

// 后端枚举：loader u8 / type u8，下标即枚举值
const LOADER_ICON = [grassBlockImg, fabricImg, anvilImg, neoforgeImg];
const TYPE_ICON = [grassBlockImg, dirtImg, cobblestoneImg, cobblestoneImg];
const TYPE_LABEL = ['Release', 'Snapshot', 'Old alpha', 'Old beta', 'Unknown'];
const LOADER_LABEL = ['Vanilla', 'Fabric', 'Forge', 'NeoForge'];

const FILTERS = [
    { key: 'all', label: 'All versions' },
    { key: 'release', label: 'Release' },
    { key: 'snapshot', label: 'Snapshot' },
    { key: 'old', label: 'Old alpha / beta' },
];

let remoteVersions = [];   // 原版完整清单（新 → 旧）
let filterKey = 'all';
let searchQuery = '';
let currentVersion = null; // 详情视图对应的原版版本
let installing = false;    // 本页安装锁（与后端 INSTALLING 双保险）

// ── DOM refs ─────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);

let loaderIconImg = null;   // loader select 关闭态前导图标
let lvIconImg = null;       // loader version select 前导图标

// ── 工具 ─────────────────────────────────────────────────────────────

// md-select 关闭态文本（displayText）是实时查询选项 headline slot 得到的；
// 选项刚插入 DOM、首帧还没渲染完（renderRoot 为 undefined）时赋值 .value
// 会把 displayText 固定为空。所以先等所有选项 updateComplete，再赋值校验，
// 必要时重设一次（与 home.js 的修复一致）。
async function setSelectValue(select, value) {
    const options = [...select.querySelectorAll('md-select-option')];
    await Promise.all(options.map((opt) => opt.updateComplete));
    select.value = value;
    await select.updateComplete;
    if (select.value !== value || !select.displayText) {
        select.value = value;
        await select.updateComplete;
    }
}

// 关闭态显示图标：img 挂到 select 的 leading-icon slot，按 loader 换图。
// img 始终保留（有默认 src），避免触发展开态的 has-start 缩进切换。
function attachLeadingIcon(select) {
    const img = document.createElement('img');
    img.slot = 'leading-icon';
    img.alt = '';
    img.src = grassBlockImg;
    select.appendChild(img);
    return img;
}

function syncLeadingIcon(img, loader) {
    if (img) {
        img.src = LOADER_ICON[loader] || musicDiscImg;
    }
}

function typeIcon(type) {
    return TYPE_ICON[type] || musicDiscImg;
}

// 已安装版本图标：loader 优先（后端 detect_loader 结果），
// vanilla 再按 type，rd-/a/b/c/inf 前缀的老版本用 cobblestone。
function versionIcon(ver) {
    if (ver.loader) {
        return LOADER_ICON[ver.loader] || musicDiscImg;
    }
    const name = ver.name || '';
    if (/^(rd-|a\d|b\d|c\d|inf-)/.test(name)) {
        return cobblestoneImg;
    }
    return typeIcon(ver.type);
}

// 与后端一致的 Windows 文件名非法字符校验
function validInstanceName(name) {
    return !/[/\\:*?"<>|]/.test(name) && !name.includes('..');
}

function setError(msg) {
    const el = $('dl-error');
    el.textContent = msg;
    el.hidden = !msg;
}

function hideProgress() {
    $('dl-progress').hidden = true;
}

// ── 视图 1：远程版本列表 ─────────────────────────────────────────────

function remoteTypeFilter(ver) {
    if (filterKey === 'release') return ver.type === 0;
    if (filterKey === 'snapshot') return ver.type === 1;
    if (filterKey === 'old') return ver.type === 2 || ver.type === 3;
    return true;
}

function filteredRemote() {
    const q = searchQuery.toLowerCase();
    return remoteVersions.filter((v) => remoteTypeFilter(v)
        && (!q || v.name.toLowerCase().includes(q)));
}

function remoteCard(ver) {
    const label = TYPE_LABEL[ver.type] ?? 'Unknown';
    return `
        <li class="dl-card" data-name="${escapeHtml(ver.name)}">
            <img src="${typeIcon(ver.type)}" alt="" />
            <div class="dl-card-text">
                <span class="dl-card-name">${escapeHtml(ver.name)}</span>
                <span class="dl-card-type">${label}</span>
            </div>
        </li>`;
}

function renderRemoteList() {
    const items = filteredRemote();
    $('dl-remote-list').innerHTML = items.map(remoteCard).join('');
    $('dl-remote-empty').hidden = items.length !== 0;
}

async function loadRemote() {
    $('dl-loading').hidden = false;
    $('dl-remote-empty').hidden = true;
    try {
        remoteVersions = await invokeFn('list_remote_versions', { loader: 0 });
    } catch (error) {
        console.error('Failed to load remote versions', error);
        remoteVersions = [];
        $('dl-remote-empty').textContent
            = 'Failed to load versions. Check your connection and reopen the page.';
    }
    $('dl-loading').hidden = true;
    renderRemoteList();
}

// ── 视图 2：详情（loader 选择 + 自定义名 + 安装） ─────────────────────

function hideLoaderVersion() {
    $('dl-loader-version-select').hidden = true;
    $('dl-loader-hint').hidden = true;
    lvIconImg.hidden = true;
}

async function onLoaderChange(loader) {
    const lvSelect = $('dl-loader-version-select');
    const hint = $('dl-loader-hint');
    const idx = Number(loader);
    syncLeadingIcon(loaderIconImg, idx);
    if (idx === 0) {
        // vanilla：不需要 loaderVersion
        hideLoaderVersion();
        return;
    }
    lvSelect.hidden = false;
    lvSelect.disabled = true;
    lvSelect.innerHTML = '';
    lvIconImg.hidden = false;
    syncLeadingIcon(lvIconImg, idx);
    hint.hidden = false;
    hint.textContent = `Loading ${LOADER_LABEL[idx]} versions…`;
    let list = [];
    try {
        list = await invokeFn('list_remote_versions', {
            loader: idx,
            gameVersion: currentVersion.name,
        });
    } catch (error) {
        hint.textContent = `${LOADER_LABEL[idx]} is not available for ${currentVersion.name}.`;
        console.error('Failed to load loader versions', error);
        return;
    }
    if (!list.length) {
        hint.textContent = `No ${LOADER_LABEL[idx]} builds for ${currentVersion.name}.`;
        return;
    }
    hint.hidden = true;
    lvSelect.innerHTML = list.map((v) => `
        <md-select-option value="${escapeHtml(v.name)}">
            <img slot="start" src="${LOADER_ICON[idx]}" alt="" />
            <span slot="headline">${escapeHtml(v.name)}</span>
        </md-select-option>
    `).join('');
    lvSelect.disabled = false;
    // 默认选最新构建
    await setSelectValue(lvSelect, list[0].name);
}

async function openDetail(ver) {
    currentVersion = ver;
    $('dl-browser').hidden = true;
    $('dl-detail').hidden = false;
    $('dl-detail-title').textContent = ver.name;
    const nameField = $('dl-custom-name');
    nameField.value = ver.name;
    nameField.error = false;
    setError('');
    hideProgress();
    hideLoaderVersion();
    // 重置 loader 选择为 vanilla（static options 也要等渲染完才能赋值）
    await setSelectValue($('dl-loader-select'), '0');
    syncLeadingIcon(loaderIconImg, 0);
    if (installing) {
        // 安装进行中重进详情：直接显示进度区
        $('dl-progress').hidden = false;
    }
}

function closeDetail() {
    $('dl-detail').hidden = true;
    $('dl-browser').hidden = false;
    currentVersion = null;
    hideProgress();
    setError('');
}

function setInstalling(flag) {
    installing = flag;
    $('dl-install-btn').disabled = flag;
    $('dl-cancel-btn').hidden = !flag;
    if (flag) {
        const bar = $('dl-progress-bar');
        bar.indeterminate = true;
        bar.value = 0;
        $('dl-progress').hidden = false;
        $('dl-progress-label').textContent = 'Starting install…';
    }
}

async function startInstall() {
    if (installing || !currentVersion) {
        return;
    }
    const idx = Number($('dl-loader-select').value);
    const lvSelect = $('dl-loader-version-select');
    const loaderVersion = lvSelect.hidden ? null : (lvSelect.value || null);
    if (idx !== 0 && !loaderVersion) {
        setError(`Pick a ${LOADER_LABEL[idx]} version first.`);
        return;
    }
    const nameField = $('dl-custom-name');
    const custom = nameField.value.trim();
    if (custom && !validInstanceName(custom)) {
        nameField.error = true;
        nameField.errorText = 'Name cannot contain / \\ : * ? " < > |';
        return;
    }
    // 与版本名相同（或留空）→ 交给后端用默认 id 命名
    const customName = custom && custom !== currentVersion.name ? custom : null;
    setError('');
    setInstalling(true);
    try {
        const installed = await invokeFn('install_version', {
            gameVersion: currentVersion.name,
            loader: idx,
            loaderVersion,
            javaPath: null,
            customName,
        });
        const bar = $('dl-progress-bar');
        bar.indeterminate = false;
        bar.value = 1;
        $('dl-progress-label').textContent = `Installed as ${installed.name}`;
    } catch (error) {
        setError(String(error));
        hideProgress();
    } finally {
        setInstalling(false);
        await refreshInstalled();
    }
}

// 字节数人性化显示：0 时不显示；'12.3 KB' / '4.5 MB' / '1.2 GB'
function fmtBytes(n) {
    if (!Number.isFinite(n) || n <= 0) {
        return '';
    }
    const units = ['B', 'KB', 'MB', 'GB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
        v /= 1024;
        i += 1;
    }
    return i === 0 ? `${v} B` : `${v.toFixed(1)} ${units[i]}`;
}

// install-progress 负载：
//   { name, stage, done, total, bytesDone, bytesTotal, speed, label }
//   done/total：libraries/natives/assets 为文件数；client/installer 阶段为字节数
//   bytesDone/bytesTotal：字节级进度（0=未知）；speed：估算速度（字节/秒）
function onProgress(event) {
    const p = event.payload || {};
    const { stage, done = 0, total = 0, label } = p;
    const bytesDone = p.bytesDone || 0;
    const bytesTotal = p.bytesTotal || 0;
    const speed = p.speed || 0;
    const bar = $('dl-progress-bar');
    if (stage === 'done') {
        bar.indeterminate = false;
        bar.value = 1;
        $('dl-progress-label').textContent = label || 'Done';
        return;
    }
    if (!installing) {
        setInstalling(true);
    }
    // 进度条：优先字节进度（更平滑），其次文件数，都没有则不确定
    if (bytesTotal > 0) {
        bar.indeterminate = false;
        bar.value = Math.min(1, bytesDone / bytesTotal);
    } else if (total > 0) {
        bar.indeterminate = false;
        bar.value = Math.min(1, done / total);
    } else {
        bar.indeterminate = true;
    }
    // 文字：阶段文件数 · 已下字节/总字节 · 速度
    const bits = [];
    if (stage === 'client') {
        // client 阶段 done/total 本身就是字节数
        const d = fmtBytes(done) || fmtBytes(bytesDone);
        const t = fmtBytes(total) || fmtBytes(bytesTotal);
        bits.push(t ? `client ${d} / ${t}` : `client ${d}`);
    } else {
        bits.push(`${stage} ${done}/${total}`);
        const bd = fmtBytes(bytesDone);
        const bt = fmtBytes(bytesTotal);
        if (bt) {
            bits.push(bd ? `${bd} / ${bt}` : bt);
        }
    }
    if (speed > 0) {
        bits.push(`${fmtBytes(speed)}/s`);
    }
    $('dl-progress-label').textContent
        = [label || '下载中…', bits.join(' · ')].filter(Boolean).join(' · ');
}

// ── 已安装版本 ───────────────────────────────────────────────────────

async function refreshInstalled() {
    const listEl = $('version-list');
    if (!listEl) {
        // Installed 区块不在页面上（download.html 里被注释掉了），跳过
        return;
    }
    let versions = [];
    try {
        versions = await invokeFn('get_local_versions');
    } catch (error) {
        console.error('Failed to load local versions', error);
    }
    listEl.innerHTML = versions.map((ver) => `
        <li class="version">
            <img src="${versionIcon(ver)}" alt="" />
            <h3 title="${escapeHtml(ver.name)}">${escapeHtml(ver.name)}</h3>
            <p class="muted">${escapeHtml(ver.official_name !== ver.name ? ver.official_name : '')}</p>
        </li>
    `).join('');
    $('dl-installed-empty').hidden = versions.length !== 0;
}

// ── 初始化 ───────────────────────────────────────────────────────────

function bindEvents() {
    // 搜索
    const search = $('dl-search');
    search.addEventListener('input', () => {
        searchQuery = search.value.trim();
        renderRemoteList();
    });

    // 类型筛选菜单
    const filterBtn = $('dl-filter-btn');
    const filterMenu = $('dl-filter-menu');
    filterBtn.addEventListener('click', () => filterMenu.show());
    filterMenu.addEventListener('click', (e) => {
        const item = e.target.closest('md-menu-item');
        if (!item) {
            return;
        }
        filterKey = item.dataset.filter || 'all';
        $('dl-filter-label').textContent
            = FILTERS.find((f) => f.key === filterKey)?.label ?? 'All versions';
        renderRemoteList();
    });

    // 卡片点击
    $('dl-remote-list').addEventListener('click', (e) => {
        const card = e.target.closest('.dl-card');
        if (!card || installing) {
            return;
        }
        const ver = remoteVersions.find((v) => v.name === card.dataset.name);
        if (ver) {
            openDetail(ver);
        }
    });

    // 详情视图
    $('dl-back-btn').addEventListener('click', closeDetail);
    $('dl-loader-select').addEventListener('change', () => onLoaderChange($('dl-loader-select').value));
    $('dl-install-btn').addEventListener('click', startInstall);
    $('dl-cancel-btn').addEventListener('click', () => {
        invokeFn('cancel_install').catch((error) => console.error(error));
    });

    // 安装进度（全局事件，无论在哪个视图都同步到详情进度区）
    void onPageEvent('install-progress', (payload) => onProgress({ payload }));
}

async function initVersionsPage() {
    const loaderSelect = $('dl-loader-select');
    const lvSelect = $('dl-loader-version-select');
    loaderIconImg = attachLeadingIcon(loaderSelect);
    lvIconImg = attachLeadingIcon(lvSelect);
    lvIconImg.hidden = true;
    bindEvents();
    await Promise.all([loadRemote(), refreshInstalled()]);
    // 初始 displayText（vanilla）
    await setSelectValue(loaderSelect, '0');
}

document.addEventListener('DOMContentLoaded', () => {
    if (!isPage('download.html')) {
        return;
    }
    void initVersionsPage();
});

// ── Global bridge ────────────────────────────────────────────────────
window.initVersionsPage = initVersionsPage;