// ── Home page module ──────────────────────────────────────────────────
// home.html (root) loads this module. It wires up the Home
// launcher: account selection, player info, skin/cape switching, version
// selection and launching.

import {
    isPage,
    escapeHtml,
    invokeFn,
    MAX_PLAYER_NAME,
    ensureLauncherState,
    refreshInstalledVersions,
} from './main.js';

// Example skin preview source (mineskin.eu). Format:
//   body  → https://mineskin.eu/armor/body/{suite}/{size}.png
//   avatar→ https://mineskin.eu/helm/{name}/64.png
const SKIN_SIZE = 100;
const DEFAULT_ACCOUNT = 'steve';

// Track the currently selected account + skin/cape suite.
let currentAccount = DEFAULT_ACCOUNT;
let currentSkin = 'steve';
let currentCape = 'none';

function bodyUrl(suite, size = SKIN_SIZE) {
    return `https://mineskin.eu/armor/body/${encodeURIComponent(suite)}/${size}.png`;
}

function helmUrl(name, size = 64) {
    return `https://mineskin.eu/helm/${encodeURIComponent(name)}/${size}.png`;
}

// Renders every account avatar (keeps example list in sync).
function refreshAccountAvatars() {
    document.querySelectorAll('#accounts-list .account-item').forEach((item) => {
        const name = item.dataset.account;
        const img = item.querySelector('img[slot="start"]');
        if (img) {
            img.src = helmUrl(name);
        }
    });
}

// Chooses an account: updates selection highlight, player name, skin and
// the player-name field. Exposed on window for inline `onclick`.
function chooseAccount(name) {
    if (!name) {
        return;
    }

    currentAccount = name;
    currentSkin = name;
    currentCape = 'none';

    // Highlight selected account row.
    document.querySelectorAll('#accounts-list .account-item').forEach((item) => {
        const isSelected = item.dataset.account === name;
        item.classList.toggle('selected', isSelected);
        item.setAttribute('aria-selected', String(isSelected));
    });

    const username = document.getElementById('username');
    const skinImg = document.getElementById('skin');
    const nameInput = document.getElementById('home-player-name');

    if (username) {
        username.textContent = name;
    }
    if (skinImg) {
        skinImg.src = bodyUrl(name);
    }
    if (nameInput) {
        nameInput.value = name;
        localStorage.setItem('PLAYER_NAME', name);
    }

    // Sync selected skin/cape pills.
    markPill('skin-options', currentSkin);
    markPill('cape-options', currentCape);
}

// Marks the selected pill within a toggle group.
function markPill(groupId, value) {
    const group = document.getElementById(groupId);
    if (!group) {
        return;
    }
    group.querySelectorAll('.pill').forEach((pill) => {
        const pillValue = pill.dataset.skin || pill.dataset.cape;
        pill.classList.toggle('selected', pillValue === value);
    });
}

// Rebuilds the full player preview from the current account + skin/cape.
// The cape is rendered through the mineskin suite (e.g. "steve,minecon").
function refreshPlayerPreview() {
    const skinImg = document.getElementById('skin');
    if (!skinImg) {
        return;
    }
    const suite = currentCape === 'none' ? currentSkin : `${currentSkin},${currentCape}`;
    skinImg.src = bodyUrl(suite);
}

async function initHomePage() {
    const status = document.getElementById('home-status');
    const mcPathText = document.getElementById('home-mc-path');
    const nameInput = document.getElementById('home-player-name');
    const versionSelect = document.getElementById('home-version-select');
    const refreshButton = document.getElementById('home-refresh-btn');
    const startButton = document.getElementById('home-start-btn');

    if (!status || !mcPathText || !nameInput || !versionSelect || !refreshButton || !startButton) {
        return;
    }

    const renderVersionOptions = () => {
        const versions = JSON.parse(localStorage.getItem('MC_VERSIONS') || '[]');
        const selectedVersion = localStorage.getItem('MC_SELECTED_VERSION');

        if (versions.length === 0) {
            versionSelect.innerHTML = '<option value="">No installed versions found</option>';
            return;
        }

        versionSelect.innerHTML = versions.map((version) => `
            <option value="${escapeHtml(version)}" ${version === selectedVersion ? 'selected' : ''}>
                ${escapeHtml(version)}
            </option>
        `).join('');
    };

    // Seed player name from localStorage (or the default account) once.
    const storedName = localStorage.getItem('PLAYER_NAME');
    if (storedName) {
        nameInput.value = storedName;
    } else {
        localStorage.setItem('PLAYER_NAME', currentAccount);
    }

    mcPathText.textContent = localStorage.getItem('MC_PATH') || 'Not set';
    renderVersionOptions();

    // Player name edit.
    nameInput.addEventListener('input', () => {
        const name = nameInput.value.trim().slice(0, MAX_PLAYER_NAME);
        localStorage.setItem('PLAYER_NAME', name || 'Player');
        const username = document.getElementById('username');
        if (username && name) {
            username.textContent = name;
        }
    });

    // Version selection.
    versionSelect.addEventListener('change', () => {
        localStorage.setItem('MC_SELECTED_VERSION', versionSelect.value);
        status.textContent = '';
    });

    // Refresh versions.
    refreshButton.addEventListener('click', async () => {
        status.textContent = 'Refreshing versions...';
        await refreshInstalledVersions();
        renderVersionOptions();
        status.textContent = 'Versions refreshed.';
    });

    // Skin / cape toggle (delegated).
    const skinOptions = document.getElementById('skin-options');
    if (skinOptions) {
        skinOptions.addEventListener('click', (e) => {
            const pill = e.target.closest('.pill');
            if (!pill || !pill.dataset.skin) {
                return;
            }
            currentSkin = pill.dataset.skin;
            markPill('skin-options', currentSkin);
            refreshPlayerPreview();
        });
    }

    const capeOptions = document.getElementById('cape-options');
    if (capeOptions) {
        capeOptions.addEventListener('click', (e) => {
            const pill = e.target.closest('.pill');
            if (!pill || pill.dataset.cape === undefined) {
                return;
            }
            currentCape = pill.dataset.cape;
            markPill('cape-options', currentCape);
            refreshPlayerPreview();
        });
    }

    // Start Minecraft.
    startButton.addEventListener('click', async () => {
        const mcPath = localStorage.getItem('MC_PATH') || '';
        const version = versionSelect.value;
        const playerName = (nameInput.value || currentAccount).trim().slice(0, MAX_PLAYER_NAME) || currentAccount;

        if (!version) {
            status.textContent = 'Please pick a version first.';
            return;
        }

        startButton.disabled = true;
        status.textContent = 'Starting Minecraft...';
        try {
            const result = await invokeFn('start_minecraft', { mcPath, version, playerName });
            status.textContent = result;
        } catch (error) {
            status.textContent = `Failed to start Minecraft: ${error}`;
        } finally {
            startButton.disabled = false;
        }
    });

    // Initial render.
    refreshAccountAvatars();
    chooseAccount(storedName || currentAccount);
}

// ── Add Account dialog (添加账号对话框) ────────────────────────────────
// Offline / Microsoft 通过 md-tabs 切换。真实登录 / 账号存储逻辑未实现，
// 由 addAccount() 占位函数预留（含 JS + Rust Tauri command 接入点）。

const ACCOUNT_MODE_OFFLINE = 'offline';
const ACCOUNT_MODE_MS = 'microsoft';
let addAccountMode = ACCOUNT_MODE_OFFLINE;

// Wire the "Add account" button → dialog, plus tab + confirm handling.
function setupAddAccountDialog() {
    const dialog = document.getElementById('add-account-dialog');
    const openBtn = document.getElementById('add-account-btn');
    const tabs = document.getElementById('add-account-tabs');

    if (!dialog || !openBtn) {
        return;
    }

    openBtn.addEventListener('click', () => {
        resetAddAccountDialog();
        dialog.show();
    });

    // Tab switching → show/hide the matching content panel.
    if (tabs) {
        tabs.addEventListener('change', () => {
            for (const tab of tabs.querySelectorAll('md-primary-tab')) {
                if (tab.active) {
                    setAddAccountMode(tab.dataset.mode || ACCOUNT_MODE_OFFLINE);
                }
            }
        });
    }

    // Confirm: gather input and hand off to the placeholder handler.
    const confirmBtn = dialog.querySelector('md-button[slot="confirm"]');
    if (confirmBtn) {
        confirmBtn.addEventListener('click', () => {
            const payload = collectAddAccountInput();
            if (payload) {
                addAccount(payload);
            }
        });
    }
}

// Switches the visible panel for the given mode.
function setAddAccountMode(mode) {
    addAccountMode = mode;
    const offlinePanel = document.getElementById('add-account-offline');
    const msPanel = document.getElementById('add-account-microsoft');
    if (offlinePanel) {
        offlinePanel.hidden = mode !== ACCOUNT_MODE_OFFLINE;
    }
    if (msPanel) {
        msPanel.hidden = mode !== ACCOUNT_MODE_MS;
    }
}

// Resets dialog state each time it opens.
function resetAddAccountDialog() {
    addAccountMode = ACCOUNT_MODE_OFFLINE;
    const tabs = document.getElementById('add-account-tabs');
    if (tabs) {
        tabs.querySelectorAll('md-primary-tab').forEach((tab, index) => {
            tab.active = index === 0;
        });
    }
    setAddAccountMode(ACCOUNT_MODE_OFFLINE);

    const name = document.getElementById('offline-name');
    if (name) {
        name.value = '';
        name.removeAttribute('error');
    }
    const code = document.getElementById('ms-code');
    if (code) {
        code.value = '';
        code.removeAttribute('error');
    }
}

// Reads + validates the dialog fields. Returns the account payload to hand
// off to addAccount(), or null when required input is missing.
function collectAddAccountInput() {
    if (addAccountMode === ACCOUNT_MODE_OFFLINE) {
        const name = document.getElementById('offline-name');
        const value = name ? name.value.trim() : '';
        if (name) {
            name.toggleAttribute('error', value.length === 0);
        }
        if (!value) {
            return null;
        }
        return { mode: ACCOUNT_MODE_OFFLINE, name: value };
    }

    const code = document.getElementById('ms-code');
    const value = code ? code.value.trim() : '';
    if (code) {
        code.toggleAttribute('error', value.length === 0);
    }
    if (!value) {
        return null;
    }
    // TODO(js/rust): 微软账号在此需要把授权码/令牌交给 Rust OAuth 命令换取账号信息。
    return { mode: ACCOUNT_MODE_MS, code: value };
}

// PLACEHOLDER — 账号添加的真实 JS/Rust 逻辑未实现。
// TODO(impl): 在此返回 Promise，调用 Rust Tauri command（如 add_account / msa_login），
// 成功后把账号写入 localStorage/账户表、刷新 #accounts-list 与 #account-info，
// 再关闭对话框。当前仅打印 payload、不做任何账号存储。
function addAccount(payload) {
    console.log('[addAccount] PLACEHOLDER — received:', payload);
}

document.addEventListener('DOMContentLoaded', async () => {
    if (!isPage('home.html')) {
        return;
    }
    await ensureLauncherState();
    await refreshInstalledVersions();
    await initHomePage();
    setupAddAccountDialog();
});

// ── Global bridge ────────────────────────────────────────────────────
window.initHomePage = initHomePage;
window.chooseAccount = chooseAccount;
window.addAccount = addAccount;