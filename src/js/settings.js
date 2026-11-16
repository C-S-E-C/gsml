// ── Settings page module ───────────────────────────────────────────────
// settings.html loads this module. It owns the settings sidebar (category
// list on the left) and the content pane on the right. Only the "About"
// view exists for now; more views can be added to the list / #view-* panels.

import { isPage,invokeFn,tauriapp,getDeviceIdentifier,escapeHtml,onPageEvent } from './main.js';
import { checkForUpdates, downloadUpdate, applyDownloaded, onUpdateProgress } from './updater.js';

const VIEWS = [
    { key: 'about' },
    { key: 'update' },
    { key: 'java' },
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
    // 更新源本身改成列表（update-sources / update-source-active），
    // 旧的单源键 auto-update-server / custom-update-server-url 已弃用
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
    const source = activeUpdateSource();
    const customUrl = source?.url || null;
    try {
        button.disabled = true;
        updateStatus('Checking for updates…');
        const info = await checkForUpdates(customUrl, source?.append_per_page !== false);
        if (info.up_to_date) {
            updateStatus(`Already up to date (${info.latest_version})`);
            return;
        }
        const channel = info.is_prerelease ? ' (pre-release)' : '';
        // 不强更：发现新版本先用 M3 对话框征求同意，拒绝则停在提示页
        const confirmed = await showUpdateConfirmDialog(
            `Update available: ${info.latest_version}${channel} (${info.asset_name}).` +
            '\nDownload and install it now? The launcher will restart.',
        );
        if (!confirmed) {
            updateStatus(`Update ${info.latest_version}${channel} skipped; check again later.`);
            return;
        }
        updateStatus(`Downloading ${info.asset_name}…`, {
            showProgress: true,
            progress: 0,
        });
        const file = await downloadUpdate(({ done, total, label }) => {
            updateStatus(label || 'Downloading…', {
                showProgress: true,
                progress: total > 0 ? done / total : 0,
            });
        });
        await applyDownloaded(file);
    } catch (err) {
        console.error('update failed:', err);
        updateStatus(`Update failed: ${err}`);
    } finally {
        button.disabled = false;
    }
}

async function initUpdateSettings() {
    const toggle = document.getElementById('auto-update-toggle');
    const button = document.getElementById('check-update-button');
    const versionEl = document.getElementById('update-current-version');
    if (!toggle) return;

    // 恢复持久化偏好
    toggle.selected = (await loadSvar(SVAR.enabled, true)) !== false;
    if (versionEl) versionEl.textContent = await tauriapp.getVersion();

    toggle.addEventListener('change', () => saveSvar(SVAR.enabled, toggle.selected));
    if (button) button.addEventListener('click', runCheck);
    // 更新源列表（与下载源同款面板）：加载 + 竞速测一次
    await initUpdateSourcePanel();
    // 后台进度（auto-update 轮询等场景未来复用）
    onUpdateProgress(() => {});
}

// ── Java 设置区（view-java）──────────────────────────────────────────
// 自定义 Java 路径 + 自动扫描到的 Java 运行时列表。
// 持久化：simpleValues，键 'java-path'；值为空字符串表示走自动检测。
const JAVA_SVAR_KEY = 'java-path';

let javaRuntimes = [];

function javaStatusEl() {
    return document.getElementById('java-status');
}

function javaStatus(text, isError = false) {
    const el = javaStatusEl();
    if (!el) return;
    el.textContent = text;
    el.classList.toggle('java-status--error', isError);
    el.hidden = !text;
}

function javaActivePath() {
    const input = document.getElementById('java-path-input');
    return (input?.value || '').trim();
}

function javaItemHtml(rt) {
    const active = rt.is_default || (javaActivePath() && javaActivePath() === rt.path);
    // 标题格式：「JDK/JRE 版本号 · 厂商」（如 JDK 21.0.4 · Eclipse Temurin）。
    const title = `${rt.kind} ${rt.version}`;
    const end = active
        ? `<md-icon slot="end" class="java-item-active">check_circle</md-icon>`
        : `<md-filled-tonal-button slot="end" class="java-use" data-path="${escapeHtml(rt.path)}">Use</md-filled-tonal-button>`;
    return `
        <md-list-item type="button" class="java-item${active ? ' java-item--active' : ''}" data-path="${escapeHtml(rt.path)}">
            <md-icon slot="start">local_cafe</md-icon>
            <div slot="headline">${escapeHtml(title)}</div>
            <div slot="supporting-text" class="java-item-path">${escapeHtml(rt.path)}</div>
            ${end}
        </md-list-item>`;
}

async function renderJavaList() {
    const list = document.getElementById('java-list');
    if (!list) return;
    list.innerHTML = `<md-list-item disabled><div slot="headline" class="java-item-loading">Scanning installed Java…</div></md-list-item>`;
    try {
        javaRuntimes = await invokeFn('list_java_installations');
        if (!Array.isArray(javaRuntimes) || javaRuntimes.length === 0) {
            // 后端在游戏运行时会跳过扫描（不起 java -version 进程），这里区分提示
            const running = await invokeFn('is_game_running').catch(() => false);
            const text = running
                ? 'Minecraft is running, so the Java scan was skipped (stop the game and refresh).'
                : 'No installed Java found.';
            list.innerHTML = `<md-list-item disabled><div slot="headline" class="java-item-loading">${text}</div></md-list-item>`;
            return;
        }
        list.innerHTML = javaRuntimes.map(javaItemHtml).join('');
    } catch (err) {
        console.error('list_java_installations failed:', err);
        list.innerHTML = `<md-list-item disabled><div slot="headline" class="java-item-loading">Scan failed: ${escapeHtml(String(err))}</div></md-list-item>`;
    }
}

async function saveJavaPath(value) {
    try {
        await invokeFn('set_svar', { key: JAVA_SVAR_KEY, value });
    } catch (err) {
        console.error(`set_svar(${JAVA_SVAR_KEY}) failed:`, err);
        javaStatus(`Save failed: ${err}`, true);
        return false;
    }
    return true;
}

async function initJavaSettings() {
    const input = document.getElementById('java-path-input');
    const saveButton = document.getElementById('java-save-button');
    const clearButton = document.getElementById('java-clear-button');
    const list = document.getElementById('java-list');
    if (!input || !saveButton || !list) return;

    input.value = (await loadSvar(JAVA_SVAR_KEY, '')) || '';
    await renderJavaList();

    saveButton.addEventListener('click', async () => {
        const raw = input.value.trim();
        saveButton.disabled = true;
        try {
            if (!raw) {
                if (await saveJavaPath('')) javaStatus('Custom Java cleared; back to auto-detect.');
                await renderJavaList();
                return;
            }
            javaStatus('Validating this Java…');
            const rt = await invokeFn('check_java_path', { path: raw });
            if (await saveJavaPath(rt.path)) {
                input.value = rt.path;
                javaStatus(`Saved: ${[rt.kind, rt.version].filter(Boolean).join(' ')}${rt.vendor ? ` (${rt.vendor})` : ''} — ${rt.path}`);
            }
            await renderJavaList();
        } catch (err) {
            javaStatus(String(err), true);
            await renderJavaList();
        } finally {
            saveButton.disabled = false;
        }
    });

    clearButton?.addEventListener('click', async () => {
        input.value = '';
        if (await saveJavaPath('')) javaStatus('Custom Java cleared; back to auto-detect.');
        await renderJavaList();
    });

    // 列表内 "Use" 按钮：把该运行时设为启动/安装使用的 Java。
    list.addEventListener('click', async (e) => {
        const button = e.target.closest('.java-use');
        if (!button) return;
        const path = button.dataset.path;
        if (!path) return;
        if (await saveJavaPath(path)) {
            input.value = path;
            const rt = javaRuntimes.find((r) => r.path === path);
            javaStatus(`Saved: ${[rt?.kind, rt?.version].filter(Boolean).join(' ') || 'Java'} — ${path}`);
            await renderJavaList();
        }
    });
}

// ── Download sources（多源同时测速竞速 / 可自定义）─────────────────────
const SRC_KEY = 'download-sources';
const SRC_INTERVAL_KEY = 'download-source-interval';
const SRC_PAUSE_KEY = 'download-source-pause-when-running';
const SRC_ACTIVE_KEY = 'download-source-active';

const $el = (id) => document.getElementById(id);

let sources = [];
let intervalMin = 5;
let pauseWhilePlaying = true;
let sourceEntries = [];
let chosenSourceId = null;
let testingSources = false;

function setSourceNote(text) {
    const el = $el('dl-sources-note');
    if (el) {
        el.textContent = text || '';
    }
}

function renderSources() {
    const list = $el('dl-source-list');
    if (!list) {
        return;
    }
    list.innerHTML = sources.map((src) => {
        const entry = sourceEntries.find((e) => e.id === src.id);
        let latency = 'Testing';
        let cls = 'idle';
        if (src.enabled === false) {
            latency = 'Disabled';
        } else if (entry) {
            if (entry.latency_ms != null) {
                latency = `${entry.latency_ms} ms`;
                cls = 'ok';
            } else {
                latency = entry.error ? 'Unavailable' : 'Testing';
                cls = 'bad';
            }
        }
        const chosen = chosenSourceId === src.id ? ' selected' : '';
        return `
            <li class="dl-source${chosen}" data-id="${escapeHtml(src.id)}"
                title="${escapeHtml(src.manifest_url)}">
                <md-icon class="dl-source-icon">dns</md-icon>
                <span class="dl-source-name">${escapeHtml(src.name)}</span>
                <span class="dl-source-latency ${cls}">${escapeHtml(latency)}</span>
                <button class="dl-source-remove" data-id="${escapeHtml(src.id)}"
                    aria-label="Remove source" title="Remove source">×</button>
            </li>`;
    }).join('');
}

async function saveSources() {
    try {
        await invokeFn('set_svar', { key: SRC_KEY, value: sources });
    } catch (error) {
        console.error('save download sources failed', error);
    }
}

async function loadSourceSettings() {
    try {
        const [list, interval, pause] = await Promise.all([
            loadSvar(SRC_KEY, null),
            loadSvar(SRC_INTERVAL_KEY, null),
            loadSvar(SRC_PAUSE_KEY, null),
        ]);
        sources = Array.isArray(list) && list.length
            ? list.filter((s) => s && s.id && s.manifest_url)
            : [];
        if (!sources.length) {
            sources = await invokeFn('default_download_sources').catch(() => []);
        }
        intervalMin = Number.isFinite(interval) && interval >= 1
            ? Math.min(180, Math.round(interval))
            : 5;
        pauseWhilePlaying = pause !== false;
    } catch (error) {
        console.error('load download sources failed', error);
    }
    if ($el('dl-source-interval')) {
        $el('dl-source-interval').value = String(intervalMin);
    }
    if ($el('dl-source-pause')) {
        $el('dl-source-pause').checked = pauseWhilePlaying;
    }
    renderSources();
}

/**
 * 同时给所有源测速并解析清单：谁先响应且解析成功就用谁，其余丢弃。
 * 结果里带每个源的延迟，胜出的那个会被标记并写进 active svar。
 */
async function testSources() {
    if (testingSources) {
        return;
    }
    testingSources = true;
    setSourceNote('Testing all sources…');
    try {
        const res = await invokeFn('probe_download_sources', {
            sources,
            pauseWhileRunning: pauseWhilePlaying,
        });
        sourceEntries = res.entries || [];
        chosenSourceId = res.source_id ?? null;
        renderSources();
        if (res.skipped) {
            setSourceNote('Minecraft is running; skipped this test');
        } else if (res.error) {
            setSourceNote(res.error);
        } else if (res.source_name) {
            const ms = res.latency_ms != null ? ` · ${res.latency_ms} ms` : '';
            setSourceNote(`Fastest: ${res.source_name}${ms}`);
            void invokeFn('set_svar', { key: SRC_ACTIVE_KEY, value: res.source_id ?? null })
                .catch(() => {});
        } else {
            setSourceNote('');
        }
    } catch (error) {
        console.error('probe download sources failed', error);
        setSourceNote(String(error));
    } finally {
        testingSources = false;
    }
}

function bindSourcePanel() {
    if (!$el('dl-source-list')) {
        return;
    }
    $el('dl-source-refresh')?.addEventListener('click', () => {
        void testSources();
    });
    $el('dl-source-interval')?.addEventListener('change', async (e) => {
        const n = Math.min(180, Math.max(1, Number.parseInt(e.target.value, 10) || 5));
        intervalMin = n;
        e.target.value = String(n);
        await invokeFn('set_svar', { key: SRC_INTERVAL_KEY, value: n }).catch(() => {});
    });
    $el('dl-source-pause')?.addEventListener('change', async (e) => {
        pauseWhilePlaying = e.target.checked;
        await invokeFn('set_svar', { key: SRC_PAUSE_KEY, value: pauseWhilePlaying })
            .catch(() => {});
    });
    $el('dl-source-list')?.addEventListener('click', async (e) => {
        const btn = e.target.closest('.dl-source-remove');
        if (!btn) {
            return;
        }
        if (sources.length <= 1) {
            setSourceNote('Keep at least one download source');
            return;
        }
        sources = sources.filter((s) => s.id !== btn.dataset.id);
        renderSources();
        await saveSources();
        void testSources();
    });
    $el('dl-source-add')?.addEventListener('click', () => {
        $el('source-name').value = '';
        $el('source-url').value = '';
        void $el('source-dialog').show();
    });
    $el('source-cancel')?.addEventListener('click', () => {
        void $el('source-dialog').close();
    });
    $el('source-save')?.addEventListener('click', async () => {
        const name = $el('source-name').value.trim();
        const url = $el('source-url').value.trim();
        if (!name || !/^https?:\/\//i.test(url)) {
            setSourceNote('Enter a name and an http(s) manifest URL');
            return;
        }
        sources = [...sources, {
            id: `custom-${Date.now()}`,
            name,
            manifest_url: url,
            ping_url: '',
            rewrites: [],
            enabled: true,
        }];
        await saveSources();
        renderSources();
        void $el('source-dialog').close();
        void testSources();
    });
}

// ── Update sources（与下载源同款列表：多源同时竞速）─────────────────────
const UPDATE_SRC_KEY = 'update-sources';
const UPDATE_SRC_ACTIVE_KEY = 'update-source-active';

let updateSources = [];
let updateEntries = [];
let updateChosenId = null;
let testingUpdates = false;

function updateNote(text) {
    const el = $el('update-sources-note');
    if (el) {
        el.textContent = text || '';
    }
}

/** 当前选中的更新源（没测过就退回列表第一个）。 */
function activeUpdateSource() {
    return updateSources.find((s) => s.id === updateChosenId) || updateSources[0] || null;
}

function renderUpdateSources() {
    const list = $el('update-source-list');
    if (!list) {
        return;
    }
    list.innerHTML = updateSources.map((src) => {
        const entry = updateEntries.find((e) => e.id === src.id);
        let latency = 'Testing';
        let cls = 'idle';
        if (entry) {
            if (entry.latency_ms != null) {
                latency = `${entry.latency_ms} ms`;
                cls = 'ok';
            } else {
                latency = entry.error ? 'Unavailable' : 'Testing';
                cls = 'bad';
            }
        }
        const chosen = updateChosenId === src.id ? ' selected' : '';
        const perPage = src.append_per_page === false ? ' (no ?per_page=20)' : '';
        return `
            <li class="dl-source${chosen}" data-id="${escapeHtml(src.id)}"
                title="${escapeHtml(src.url)}${perPage}">
                <md-icon class="dl-source-icon">system_update</md-icon>
                <span class="dl-source-name">${escapeHtml(src.name)}</span>
                <span class="dl-source-latency ${cls}">${escapeHtml(latency)}</span>
                <button class="dl-source-remove" data-id="${escapeHtml(src.id)}"
                    aria-label="Remove source" title="Remove source">×</button>
            </li>`;
    }).join('');
}

async function saveUpdateSources() {
    try {
        await invokeFn('set_svar', { key: UPDATE_SRC_KEY, value: updateSources });
    } catch (error) {
        console.error('save update sources failed', error);
    }
}

async function loadUpdateSources() {
    try {
        const list = await loadSvar(UPDATE_SRC_KEY, null);
        updateSources = Array.isArray(list) && list.length
            ? list.filter((s) => s && s.id && s.url)
            : [];
        if (!updateSources.length) {
            updateSources = await invokeFn('default_update_sources_cmd').catch(() => []);
        }
        updateChosenId = (await loadSvar(UPDATE_SRC_ACTIVE_KEY, null)) || null;
    } catch (error) {
        console.error('load update sources failed', error);
    }
    renderUpdateSources();
}

/** 同时给所有更新源测速并解析 release 列表，先成功者胜出。 */
async function testUpdateSources() {
    if (testingUpdates) {
        return;
    }
    testingUpdates = true;
    updateNote('Testing all sources…');
    try {
        const res = await invokeFn('probe_update_sources', { sources: updateSources });
        updateEntries = res.entries || [];
        updateChosenId = res.source_id ?? updateChosenId;
        renderUpdateSources();
        if (res.error) {
            updateNote(res.error);
        } else if (res.source_name) {
            const ms = res.latency_ms != null ? ` · ${res.latency_ms} ms` : '';
            updateNote(`Fastest: ${res.source_name}${ms}`);
            void saveSvar(UPDATE_SRC_ACTIVE_KEY, res.source_id ?? null);
        } else {
            updateNote('');
        }
    } catch (error) {
        console.error('probe update sources failed', error);
        updateNote(String(error));
    } finally {
        testingUpdates = false;
    }
}

async function initUpdateSourcePanel() {
    if (!$el('update-source-list')) {
        return;
    }
    await loadUpdateSources();
    $el('update-source-refresh')?.addEventListener('click', () => {
        void testUpdateSources();
    });
    $el('update-source-list')?.addEventListener('click', async (e) => {
        const btn = e.target.closest('.dl-source-remove');
        if (!btn) {
            return;
        }
        if (updateSources.length <= 1) {
            updateNote('Keep at least one update source');
            return;
        }
        updateSources = updateSources.filter((s) => s.id !== btn.dataset.id);
        renderUpdateSources();
        await saveUpdateSources();
        void testUpdateSources();
    });
    $el('update-source-add')?.addEventListener('click', () => {
        $el('update-source-name').value = '';
        $el('update-source-url').value = '';
        if ($el('update-source-perpage')) {
            $el('update-source-perpage').checked = true;
        }
        void $el('update-source-dialog').show();
    });
    $el('update-source-cancel')?.addEventListener('click', () => {
        void $el('update-source-dialog').close();
    });
    $el('update-source-save')?.addEventListener('click', async () => {
        const name = $el('update-source-name').value.trim();
        const url = $el('update-source-url').value.trim();
        if (!name || !/^https?:\/\//i.test(url)) {
            updateNote('Enter a name and an http(s) URL');
            return;
        }
        const appendPerPage = $el('update-source-perpage')
            ? $el('update-source-perpage').checked
            : true;
        updateSources = [...updateSources, {
            id: `custom-${Date.now()}`,
            name,
            url,
            append_per_page: appendPerPage,
        }];
        await saveUpdateSources();
        renderUpdateSources();
        void $el('update-source-dialog').close();
        void testUpdateSources();
    });
    void testUpdateSources();
}

// ── Java 自动安装（Download 设置 + Java 面板一键安装）────────────────
const JAVA_CFG = {
    auto: 'java-auto-install',
    prefer: 'java-prefer',
    mirrors: 'java-mirrors',
};

/** 读配置并同步到后端（install_version 内部会读这份配置）。 */
let javaMirrors = [];      // [{ id, name, url }]
let javaMirrorProbes = []; // [{ url, latency_ms, adoptium, error }]

function renderJavaMirrors() {
    const list = $el('java-mirror-list');
    if (!list) return;
    if (!javaMirrors.length) {
        list.innerHTML = '<li class="dl-source"><span class="dl-source-name">No mirrors — using the built-in list</span></li>';
        return;
    }
    // 按测得的延迟排序展示（最快在上；不可用/未测的排后面），不改变保存顺序
    const latencyOf = (mirror) => {
        const p = javaMirrorProbes.find((x) => x.url === mirror.url);
        return p && p.latency_ms != null ? p.latency_ms : Number.MAX_SAFE_INTEGER;
    };
    const ordered = [...javaMirrors].sort((a, b) => latencyOf(a) - latencyOf(b));
    list.innerHTML = ordered.map((mirror) => {
        const probe = javaMirrorProbes.find((p) => p.url === mirror.url);
        let latency = 'Testing';
        let cls = 'idle';
        let suffix = '';
        if (probe) {
            if (probe.latency_ms != null) {
                latency = `${probe.latency_ms} ms`;
                cls = probe.adoptium ? 'ok' : 'bad';
                if (!probe.adoptium) suffix = ' · no Adoptium';
            } else {
                latency = 'Unavailable';
                cls = 'bad';
            }
        }
        return `
            <li class="dl-source" data-id="${escapeHtml(mirror.id)}"
                title="${escapeHtml(mirror.url)} (${escapeHtml(mirror.kind || 'adoptium')})">
                <md-icon class="dl-source-icon">local_cafe</md-icon>
                <span class="dl-source-name">${escapeHtml(mirror.name)}</span>
                <span class="dl-source-latency ${cls}">${escapeHtml(latency + suffix)}</span>
                <button class="dl-source-remove" data-id="${escapeHtml(mirror.id)}"
                    aria-label="Remove mirror" title="Remove mirror">×</button>
            </li>`;
    }).join('');
}

async function loadJavaMirrors() {
    const raw = await loadSvar(JAVA_CFG.mirrors, null);
    if (Array.isArray(raw) && raw.length) {
        // isoinfo 源已废弃：加载时直接丢弃（避免列表里出现永远不可用的条目）
        javaMirrors = raw
            .filter((m) => m && m.url && (m.kind || 'adoptium') !== 'isoinfo')
            .map((m) => ({
                id: m.id || `mirror-${Math.random().toString(36).slice(2, 8)}`,
                name: m.name || String(m.url).replace(/^https?:\/\//, '').split('/')[0],
                url: m.url,
                kind: m.kind || 'adoptium',
            }));
        // 有废弃条目被丢弃 → 立刻写回，避免下次再出现
        if (javaMirrors.length !== raw.length) {
            await saveSvar(JAVA_CFG.mirrors, javaMirrors);
        }
    } else {
        javaMirrors = await invokeFn('default_java_mirrors_cmd').catch(() => []);
    }
    renderJavaMirrors();
}

async function saveJavaMirrors() {
    await saveSvar(JAVA_CFG.mirrors, javaMirrors);
    await syncJavaInstallerConfig();
}

/** 并发测速所有 Java 镜像（只展示，不竞速）。 */
async function testJavaMirrors() {
    if (!javaMirrors.length) return;
    javaMirrorProbes = [];
    renderJavaMirrors();
    try {
        const res = await invokeFn('probe_java_mirrors', {
            mirrors: javaMirrors.map((m) => ({ url: m.url, kind: m.kind || 'adoptium' })),
        });
        javaMirrorProbes = Array.isArray(res) ? res : [];
    } catch (error) {
        console.error('probe java mirrors failed:', error);
    }
    renderJavaMirrors();
}

async function syncJavaInstallerConfig() {
    const auto = (await loadSvar(JAVA_CFG.auto, true)) !== false;
    const preferJdk = (await loadSvar(JAVA_CFG.prefer, 'jre')) === 'jdk';
    const mirrors = javaMirrors.map((m) => ({ url: m.url, kind: m.kind || 'adoptium' }));
    try {
        await invokeFn('configure_java_installer', {
            auto,
            preferJdk,
            mirrors,
        });
    } catch (error) {
        console.error('configure_java_installer failed:', error);
    }
    if ($el('java-auto-install')) $el('java-auto-install').selected = auto;
    if ($el('java-prefer')) $el('java-prefer').value = preferJdk ? 'jdk' : 'jre';
}

function javaInstallStatus(text, isError = false) {
    const el = $el('java-install-status');
    if (!el) return;
    el.textContent = text;
    el.hidden = !text;
    el.classList.toggle('java-status--error', isError);
}

/** 一键给所有已安装版本补齐 Java（缺失的大版本逐个下载）。 */
async function installMissingJava() {
    const button = $el('java-install-all');
    const bar = $el('java-install-bar');
    try {
        if (button) button.disabled = true;
        const list = await invokeFn('list_java_requirements').catch(() => []);
        const missing = [...new Set(
            (Array.isArray(list) ? list : [])
                .filter((v) => v && !v.satisfied)
                .map((v) => v.major),
        )].sort((a, b) => a - b);
        if (!missing.length) {
            javaInstallStatus('Every installed version already has a matching Java.');
            return;
        }
        if (bar) {
            bar.hidden = false;
            bar.value = 0;
        }
        for (let i = 0; i < missing.length; i += 1) {
            javaInstallStatus(`Installing Java ${missing[i]}… (${i + 1}/${missing.length})`);
            await invokeFn('install_java_runtime', { major: missing[i] });
            if (bar) bar.value = (i + 1) / missing.length;
        }
        javaInstallStatus(`Done: installed Java ${missing.join(', ')}.`);
        await renderJavaList();
    } catch (error) {
        console.error('install java failed:', error);
        javaInstallStatus(`Failed: ${error}`, true);
    } finally {
        if (button) button.disabled = false;
        if (bar) bar.hidden = true;
    }
}

async function initJavaInstallerSettings() {
    await loadJavaMirrors();
    await syncJavaInstallerConfig();
    $el('java-auto-install')?.addEventListener('change', async (e) => {
        await saveSvar(JAVA_CFG.auto, e.target.selected);
        await syncJavaInstallerConfig();
    });
    $el('java-prefer')?.addEventListener('change', async (e) => {
        await saveSvar(JAVA_CFG.prefer, e.target.value);
        await syncJavaInstallerConfig();
    });
    // Java 镜像列表（与下载源/更新源同款）
    $el('java-mirror-refresh')?.addEventListener('click', () => {
        void testJavaMirrors();
    });
    $el('java-mirror-list')?.addEventListener('click', async (e) => {
        const btn = e.target.closest('.dl-source-remove');
        if (!btn) return;
        javaMirrors = javaMirrors.filter((m) => m.id !== btn.dataset.id);
        renderJavaMirrors();
        await saveJavaMirrors();
    });
    $el('java-mirror-add')?.addEventListener('click', () => {
        $el('java-mirror-name').value = '';
        $el('java-mirror-url').value = '';
        if ($el('java-mirror-kind')) $el('java-mirror-kind').value = 'adoptium';
        void $el('java-mirror-dialog').show();
    });
    $el('java-mirror-cancel')?.addEventListener('click', () => {
        void $el('java-mirror-dialog').close();
    });
    $el('java-mirror-save')?.addEventListener('click', async () => {
        const name = $el('java-mirror-name').value.trim();
        const url = $el('java-mirror-url').value.trim();
        const kind = ($el('java-mirror-kind')?.value || 'adoptium').trim();
        if (!name || !/^https?:\/\//i.test(url)) {
            if ($el('java-config-status')) {
                $el('java-config-status').hidden = false;
                $el('java-config-status').textContent = 'Enter a name and an http(s) URL';
            }
            return;
        }
        javaMirrors = [...javaMirrors, { id: `custom-${Date.now()}`, name, url, kind }];
        renderJavaMirrors();
        await saveJavaMirrors();
        void $el('java-mirror-dialog').close();
        void testJavaMirrors();
    });
    $el('java-install-all')?.addEventListener('click', () => {
        void installMissingJava();
    });
    // 下载/解压 Java 的实时进度
    void onPageEvent('java-install-progress', (payload) => {
        const bar = $el('java-install-bar');
        const { phase, done, total, label } = payload || {};
        if (bar && total > 0) {
            bar.hidden = false;
            bar.value = Math.min(1, done / total);
        }
        if (label && phase !== 'done') javaInstallStatus(String(label));
    });
    // 首次进入自动测一遍，方便用户挑最快的镜像
    void testJavaMirrors();
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
    document.getElementById("device-identifier-art").innerText = drunkenBishop(await getDeviceIdentifier(), 17,33);

    // 下载源面板（Settings → Download）：加载配置并测一次速选出最快源
    bindSourcePanel();
    await loadSourceSettings();
    // Java 自动安装配置 + 「一键安装」按钮
    await initJavaInstallerSettings();
    void testSources();
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
    throw new TypeError('input must be a string');
  }
  if (!Number.isInteger(height) || height <= 0) {
    throw new RangeError('height must be a positive integer');
  }
  if (!Number.isInteger(width) || width <= 0) {
    throw new RangeError('width must be a positive integer');
  }

  // ---- 清理并校验十六进制 ----
  const hex = input.replace(/\s+/g, '').toLowerCase();
  if (!/^[0-9a-f]*$/.test(hex)) {
    throw new Error('input contains non-hex characters (only 0-9, a-f, A-F, whitespace)');
  }

  // ---- 十六进制 -> 字节数组（奇数长度左侧补 0）----
  const paddedHex = hex.length % 2 === 0 ? hex : '0' + hex;
  const bytes = [];
  for (let i = 0; i < paddedHex.length; i += 2) {
    bytes.push(parseInt(paddedHex.substr(i, 2), 16));
  }

  // ---- 初始化画布 ----
  const STEP_CHARS = [
    ' ', '·', '.', ':', ';', 'i', 'l', '!', '|', '1', 't', 'f', 'j', 'r', 'x',
    'n', 'u', 'v', 'c', 'o', 'e', 'a', 's', 'd', 'b', 'p', 'q', 'g', 'h', 'k',
    'M', '8', '@'
  ];
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
      else if (count >= 32) body += '@';
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
    initJavaSettings();
});

// ── Global bridge ────────────────────────────────────────────────────
// settings.js runs as a module, so its top-level declarations are not
// visible to inline `onclick="..."` attributes in settings.html.
window.switchSettingsView = switchSettingsView;
