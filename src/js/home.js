// ── Home page module ──────────────────────────────────────────────────
// home.html (root) loads this module. It wires up the Home page:
// dynamic account list (list_accounts), account selection + player preview,
// and the Add Account dialog (offline login / Microsoft login via Tauri
// commands). The Microsoft flow opens the system browser from Rust and the
// result is delivered through the glow-squid://ms-auth deeplink — JS never
// touches the deeplink or a manual code input, it only awaits the command.

import {
    isPage,
    escapeHtml,
    invokeFn,
    onPageEvent,
} from './main.js';

import fabricImg from '../assets/fabric.png';
import neoforgeImg from '../assets/neoforge.png';
import forgeImg from '../assets/anvil.png';
import releaseImg from '../assets/grass_block.png';
import snapshotImg from '../assets/dirt.png';
import oldImg from '../assets/cobblestone.png';
import musicDiscImg from '../assets/music_disc_11.png';

// 1×1 transparent GIF placeholder (real skin fetching is not implemented yet).
const TRANSPARENT_GIF =
    'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7';

const ACCOUNT_MODE_OFFLINE = 'offline';
const ACCOUNT_MODE_MS = 'microsoft';

// Loaded accounts + the currently selected one (null = no account).
let accounts = [];
let selectedAccountId = null;

// Add-account dialog state.
let addAccountMode = ACCOUNT_MODE_OFFLINE;
let addAccountBusy = false;

function accountTypeLabel(accountType) {
    return accountType === 0 ? 'Microsoft' : 'Offline';
}

function accountTypeIcon(accountType) {
    return accountType === 0 ? `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 23 23" fill="currentColor">
  <rect x="1" y="1" width="10" height="10"/>
  <rect x="12" y="1" width="10" height="10"/>
  <rect x="1" y="12" width="10" height="10"/>
  <rect x="12" y="12" width="10" height="10"/>
  </svg>` : `link_off`;
}

// ── Account list (dynamic rendering) ──────────────────────────────────

// Rebuilds #accounts-list from `accounts`. The trailing spacer item in
// home.html (protects the FAB from overlap) is preserved.
function renderAccounts() {
    const list = document.getElementById('accounts-list');
    if (!list) {
        return;
    }

    list.querySelectorAll('md-list-item.account-item').forEach((item) => item.remove());

    const spacer = list.querySelector('md-list-item:not(.account-item)');
    if (accounts.length === 0 || !spacer) {
        return;
    }

    const html = accounts.map((account) => `
        <md-list-item class="account-item" data-account-id="${account.id}">
            <img slot="start" src="${TRANSPARENT_GIF}" alt="${escapeHtml(account.name)}" />
            <span slot="headline">${escapeHtml(account.name)}</span>
            <span slot="supporting-text">${accountTypeLabel(account.account_type)}</span>
            <md-icon slot="end">${accountTypeIcon(account.account_type)}</md-icon>
        </md-list-item>
    `).join('');
    spacer.insertAdjacentHTML('beforebegin', html);

    markSelectedAccount();
}

// Highlights the selected row.
function markSelectedAccount() {
    document.querySelectorAll('#accounts-list .account-item').forEach((item) => {
        const isSelected = Number(item.dataset.accountId) === selectedAccountId;
        item.classList.toggle('selected', isSelected);
        item.setAttribute('aria-selected', String(isSelected));
    });
}

// Updates the player preview (#username / #skin) from the selected account.
// The skin (16×32 front view) comes from the Rust skin cache as a base64
// data URL; while loading it stays the transparent placeholder.
function renderPreview() {
    const username = document.getElementById('username');
    const skinImg = document.getElementById('skin');
    const account = accounts.find((a) => a.id === selectedAccountId) || null;

    if (username) {
        username.textContent = account ? account.name : 'No account';
    }
    // 皮肤/披风控件跟随账号类型（离线账号隐藏，微软账号拉取资料）。
    updateSkinCapeControls(account);
    if (account) {
        void loadSkinForAccount(account.id);
    } else if (skinImg) {
        skinImg.src = TRANSPARENT_GIF;
        previewRequestId++; // 取消在途的皮肤请求
    }
}

// Fetches the account list from Rust and keeps the selection valid.
// The list is ordered by last-used (COALESCE(last_used, created_at) DESC),
// so accounts[0] is the most recent account.
async function refreshAccounts() {
    try {
        accounts = await invokeFn('list_accounts');
    } catch (error) {
        console.error('Failed to list accounts', error);
        accounts = [];
    }

    if (!accounts.some((account) => account.id === selectedAccountId)) {
        selectedAccountId = accounts.length > 0 ? accounts[0].id : null;
    }
    renderAccounts();
    renderPreview();
    void loadListAvatars();
}

// Selects an account by id.
function selectAccount(accountId) {
    if (!accounts.some((account) => account.id === accountId)) {
        return;
    }
    selectedAccountId = accountId;
    markSelectedAccount();
    renderPreview();
}

// Delegated click handling for the account list.
function setupAccountList() {
    const list = document.getElementById('accounts-list');
    if (!list) {
        return;
    }
    list.addEventListener('click', (event) => {
        const item = event.target.closest('md-list-item.account-item');
        if (!item) {
            return;
        }
        selectAccount(Number(item.dataset.accountId));
    });
}

// ── Skin preview (皮肤渲染缓存) ───────────────────────────────────────
// Rust 端把皮肤渲染成 8×8 头像 + 16×32 正面全身，命令返回 base64 PNG：
//   get_account_skin({ accountId }) -> { head_base64, body_base64 }
// 缓存目录：<data>/skin/{microsoft|offline}/<uuid>/{head,body}.png

// 请求序号：切换账号后，在途的旧响应不得覆盖新预览。
let previewRequestId = 0;

// 列表头像缓存：accountId -> base64 head PNG（与预览共用一次请求）。
const listAvatarCache = new Map();

// 拉取并显示选中账号的皮肤；先重置为占位图，成功后换成 body.png。
async function loadSkinForAccount(accountId) {
    const requestId = ++previewRequestId;
    const skinImg = document.getElementById('skin');
    if (skinImg) {
        skinImg.src = TRANSPARENT_GIF;
    }
    try {
        const skin = await invokeFn('get_account_skin', { accountId });
        if (requestId !== previewRequestId) {
            return; // 已切换到别的账号，丢弃过期响应
        }
        if (skinImg && skin.body_base64) {
            skinImg.src = `data:image/png;base64,${skin.body_base64}`;
        }
        if (skin.head_base64) {
            listAvatarCache.set(accountId, skin.head_base64);
            applyListAvatars();
        }
    } catch (error) {
        if (requestId === previewRequestId && skinImg) {
            skinImg.src = TRANSPARENT_GIF;
        }
        console.error('Failed to load account skin', error);
    }
}

// ── Microsoft skin / cape (换肤 / 换披风) ─────────────────────────────
// 仅微软账号可用（account_type === 0）。ms_account_details 直接访问
// /minecraft/profile（Rust 端带缓存的 access token），返回
// { uuid, name, skins: [{state,url,variant}], capes: [{id,state,alias,url}], dbUuid }。
// uuid 是 Mojang id（无连字符），dbUuid 是数据库 uuid；换肤/换披风命令
// 只需要 accountId（数据库行 id）。换皮肤要求 64×64 PNG（Rust 校验）。

// 请求序号：切换账号后丢弃在途的 details 响应，防止串号。
let detailsRequestId = 0;

// 换肤/换披风进行中标志：禁用按钮与披风 pills，防止并发请求。
let skinCapeBusy = false;

// 选中账号的披风列表（accountId -> [{ id, alias, state }]），渲染披风 pills。
const capeOptionsCache = new Map();

// 待应用的披风选择（accountId -> '' 表示选了 None，其他为披风 id）；
// 没有键表示用户尚未改动。pill 点击只改本地状态，点 Apply Cape Changes
// 按钮才调用 ms_change_cape。
const pendingCapeIdCache = new Map();

// 披风 pill 的裁剪展示图（accountId -> Map(capeId -> dataURL)），
// 由 Rust 命令 get_cape_texture 裁剪并落盘缓存，命中时几乎即时返回。
// 注意：在途请求用 '' 占位，防止重渲染时重复发起请求；
// 占位符必须被成功或失败路径清掉，否则会被卡死、永不重试。
const capeTextureCache = new Map();

// 当前 ACTIVE 皮肤的模型（accountId -> 'slim' | 'classic'），换肤时沿用
// ——上传接口的 variant 决定新皮肤的臂宽，默认沿用现有模型。
const activeSkinVariantCache = new Map();

// 更新皮肤/披风区块的可见性与提示文案。fetchDetails 控制是否重新拉取
// 账号资料（账号切换时 true；换肤/换披风进行中为 false，避免清掉提示行、
// 重复请求资料——成功路径里 loadAccountDetails 自己会再拉一次）。
function updateSkinCapeControls(account, { fetchDetails = true } = {}) {
    const btn = document.getElementById('change-skin-btn');
    const capesLabel = document.getElementById('capes-label');
    const capeRow = document.getElementById('cape-options');
    const isMs = !!account && account.account_type === 0;
    if (btn) {
        btn.hidden = !isMs;
        btn.disabled = !isMs || skinCapeBusy;
    }
    if (capesLabel) {
        capesLabel.hidden = !isMs;
    }
    if (capeRow) {
        capeRow.hidden = !isMs;
    }
    // Apply Cape Changes 按钮随账号可见性/忙碌态同步。
    updateApplyCapeButton();
    if (fetchDetails) {
        showSkinCapeMessage('');
    }
    if (isMs && account) {
        if (fetchDetails) {
            void loadAccountDetails(account.id);
        }
    } else if (capeRow) {
        // 离线/无账号：清空披风 pills。
        capeRow.innerHTML = '';
    }
    if (!isMs) showSkinCapeMessage('Some features may be unavailable for non-Microsoft accounts.')
}

// 提示行：错误用 error 色，进行中用 on-surface-variant 色；空串隐藏。
function showSkinCapeMessage(message, isError = true) {
    const el = document.getElementById('skin-cape-message');
    if (!el) {
        return;
    }
    el.textContent = message;
    el.style.color = isError
        ? 'var(--md-sys-color-error)'
        : 'var(--md-sys-color-on-surface-variant)';
}

// 拉取微软账号资料并渲染披风 pills；成功后重新加载皮肤预览。
async function loadAccountDetails(accountId) {
    const requestId = ++detailsRequestId;
    try {
        const details = await invokeFn('ms_account_details', { accountId });
        if (requestId !== detailsRequestId) {
            return; // 已切换账号，丢弃过期响应
        }
        // Mojang 端 name 可能与本地缓存不一致，同步回列表。
        const account = accounts.find((a) => a.id === accountId);
        if (account && account.name !== details.name) {
            account.name = details.name;
            renderAccounts();
            if (accountId === selectedAccountId) {
                const username = document.getElementById('username');
                if (username) {
                    username.textContent = details.name;
                }
            }
        }
        const capes = Array.isArray(details.capes) ? details.capes : [];
        capeOptionsCache.set(accountId, capes);
        // 记录当前 ACTIVE 皮肤的模型（slim/classic），换肤时沿用。
        const activeSkin = (Array.isArray(details.skins) ? details.skins : [])
            .find((skin) => skin.state === 'ACTIVE');
        activeSkinVariantCache.set(
            accountId,
            activeSkin && String(activeSkin.variant).toUpperCase() === 'SLIM'
                ? 'slim'
                : 'classic',
        );
        if (accountId === selectedAccountId) {
            renderCapeOptions(accountId);
            // 换肤/换披风后 Rust 已重新缓存渲染图，清除头像缓存重取预览。
            listAvatarCache.delete(accountId);
            void loadSkinForAccount(accountId);
        }
        showSkinCapeMessage('');
    } catch (error) {
        if (requestId !== detailsRequestId) {
            return;
        }
        console.error('Failed to load account details', error);
        showSkinCapeMessage(String(error));
    }
}

// 渲染选中账号的披风 pills（None + 每件披风，含裁剪展示图）。
// 高亮规则：有未应用的本地选择（pendingCapeIdCache）优先高亮它；
// 否则高亮当前 ACTIVE 披风。
function renderCapeOptions(accountId) {
    const capeRow = document.getElementById('cape-options');
    if (!capeRow) {
        return;
    }
    const capes = capeOptionsCache.get(accountId) || [];
    const activeCape = capes.find((cape) => cape.state === 'ACTIVE') || null;
    const pendingCapeId = pendingCapeIdCache.get(accountId);
    const hasPending = pendingCapeIdCache.has(accountId);
    const textureByCapeId = capeTextureCache.get(accountId) || new Map();
    const html = [
        '<span class="pill cape-pill" data-cape-id="" role="button">'
        + 'None</span>',
        ...capes.map((cape) => {
            const texture = textureByCapeId.get(cape.id) || '';
            const alias = escapeHtml(cape.alias || cape.id);
            if (!texture) {
                // 占位/失败态：先显示文字 pill，图片回来后再就地换成图。
                return `<span class="pill cape-pill" data-cape-id="${escapeHtml(cape.id)}"
                    title="${alias}" role="button" aria-label="${alias}">${alias}</span>`;
            }
            const img = `<img class="cape-pill-img" src="${texture}" alt="">`;
            // 只显示披风图，无 padding 无文字；名字放 title 悬停提示。
            return `
            <span class="pill cape-pill cape-pill-textured" data-cape-id="${escapeHtml(cape.id)}"
                  title="${alias}" role="button" aria-label="${alias}">${img}</span>
        `;
        }),
    ].join('');
    capeRow.innerHTML = html;
    capeRow.querySelectorAll('.cape-pill').forEach((pill) => {
        const isSelected = hasPending
            ? pill.dataset.capeId === pendingCapeId
            : activeCape
                ? pill.dataset.capeId === activeCape.id
                : pill.dataset.capeId === '';
        pill.classList.toggle('selected', isSelected);
        pill.setAttribute('aria-selected', String(isSelected));
    });
    fillCapeTextures(accountId);
    updateApplyCapeButton();
}

// 为当前 pills 请求裁剪披风图：命中缓存先画，未命中的异步 get_cape_texture。
// 响应到达时不做代数过滤：缓存按 capeId 独立写入，DOM 就地替换对应 pill，
// 切换账号不影响（切回时 Map 里已有数据）。
function fillCapeTextures(accountId) {
    const capeRow = document.getElementById('cape-options');
    if (!capeRow) {
        return;
    }
    const capes = capeOptionsCache.get(accountId) || [];
    let textureByCapeId = capeTextureCache.get(accountId);
    if (!textureByCapeId) {
        textureByCapeId = new Map();
        capeTextureCache.set(accountId, textureByCapeId);
    }
    for (const cape of capes) {
        if (textureByCapeId.has(cape.id)) {
            continue; // 缓存命中或已在途，renderCapeOptions 已直接画上
        }
        // 先插占位，避免在途请求期间重复发起同一披风的请求。
        textureByCapeId.set(cape.id, '');
        invokeFn('get_cape_texture', {
            url: cape.url,
            alias: cape.alias || cape.id,
        })
            .then((dataUrl) => {
                if (!capeTextureCache.has(accountId)) {
                    return;
                }
                textureByCapeId.set(cape.id, dataUrl);
                const pill = capeRow.querySelector(
                    `.cape-pill[data-cape-id="${CSS.escape(cape.id)}"]`,
                );
                if (!pill) {
                    return;
                }
                const img = pill.querySelector('.cape-pill-img');
                if (img) {
                    img.src = dataUrl;
                } else {
                    // 占位期渲染的是文字 pill，图片到达后升级为图 pill。
                    pill.classList.add('cape-pill-textured');
                    pill.textContent = '';
                    pill.insertAdjacentHTML('beforeend',
                        `<img class="cape-pill-img" src="${dataUrl}" alt="">`);
                }
            })
            .catch((error) => {
                console.error('Failed to load cape texture', error);
                // 失败时移除占位，下次重渲染会再试；pill 退化为纯文字（alias）。
                if (capeTextureCache.has(accountId)) {
                    textureByCapeId.delete(cape.id);
                }
            });
    }
}

// 点击披风 pill：只记录本地待应用选择（不发网络请求），高亮 + 显示 Apply 按钮。
function handleCapePillClick(pill) {
    const account = accounts.find((a) => a.id === selectedAccountId);
    if (!account || account.account_type !== 0 || skinCapeBusy) {
        return;
    }
    const capeId = pill.dataset.capeId || '';
    const activeCape = (capeOptionsCache.get(account.id) || [])
        .find((cape) => cape.state === 'ACTIVE') || null;
    // 点了与当前 ACTIVE 相同的披风：视为无改动，清掉待应用状态。
    const sameAsActive = activeCape ? capeId === activeCape.id : capeId === '';
    if (sameAsActive) {
        pendingCapeIdCache.delete(account.id);
        showSkinCapeMessage('');
    } else {
        pendingCapeIdCache.set(account.id, capeId);
    }
    renderCapeOptions(account.id);
}

// Apply Cape Changes 按钮：把待应用的披风选择发给 Rust（None → capeId null 卸下）。
async function handleApplyCapeClick() {
    const account = accounts.find((a) => a.id === selectedAccountId);
    if (!account || account.account_type !== 0 || skinCapeBusy) {
        return;
    }
    if (!pendingCapeIdCache.has(account.id)) {
        return; // 没有待应用的改动
    }
    const capeId = pendingCapeIdCache.get(account.id) || '';
    skinCapeBusy = true;
    updateSkinCapeControls(account, { fetchDetails: false });
    showSkinCapeMessage(capeId ? 'Applying cape…' : 'Removing cape…', false);
    try {
        await invokeFn('ms_change_cape', {
            accountId: account.id,
            capeId: capeId || null,
        });
        pendingCapeIdCache.delete(account.id);
        await loadAccountDetails(account.id);
    } catch (error) {
        console.error('Failed to change cape', error);
        showSkinCapeMessage(String(error));
    } finally {
        skinCapeBusy = false;
        updateSkinCapeControls(
            accounts.find((a) => a.id === selectedAccountId) || null,
            { fetchDetails: false },
        );
        updateApplyCapeButton();
    }
}

// 更新 Apply Cape Changes 按钮可见性/禁用态。
function updateApplyCapeButton() {
    const btn = document.getElementById('apply-cape-btn');
    const account = accounts.find((a) => a.id === selectedAccountId);
    if (!btn) {
        return;
    }
    const isMs = !!account && account.account_type === 0;
    const hasPending = !!account && pendingCapeIdCache.has(account.id);
    btn.hidden = !isMs || !hasPending;
    btn.disabled = !isMs || !hasPending || skinCapeBusy;
}

// 换皮肤：Change Skin 按钮 → 动态 <input type="file" accept=".png"> 选文件，
// FileReader 读成 base64（STANDARD，带 padding）传给 ms_change_skin；
// Rust 校验 64×64 PNG（错误消息里带 64×64）。
async function handleChangeSkinClick() {
    const account = accounts.find((a) => a.id === selectedAccountId);
    if (!account || account.account_type !== 0 || skinCapeBusy) {
        return;
    }
    const file = await pickSkinPngFile();
    if (!file) {
        return; // 用户取消
    }
    const fileBase64 = await readPngAsBase64(file);
    if (!fileBase64) {
        showSkinCapeMessage('Failed to read the skin file, please try again.');
        return;
    }
    skinCapeBusy = true;
    updateSkinCapeControls(account, { fetchDetails: false });
    showSkinCapeMessage('Uploading skin…', false);
    try {
        // 模型沿用当前 ACTIVE 皮肤（slim→slim，其余→classic），Rust 端再归一化。
        const variant = activeSkinVariantCache.get(account.id) === 'slim'
            ? 'slim'
            : 'classic';
        await invokeFn('ms_change_skin', {
            accountId: account.id,
            skinBase64: fileBase64,
            variant,
        });
        await loadAccountDetails(account.id);
    } catch (error) {
        console.error('Failed to change skin', error);
        showSkinCapeMessage(String(error));
    } finally {
        skinCapeBusy = false;
        updateSkinCapeControls(
            accounts.find((a) => a.id === selectedAccountId) || null,
            { fetchDetails: false },
        );
    }
}

// 打开 PNG 文件选择器（不可写隐藏 input，一次性）；取消返回 null。
function pickSkinPngFile() {
    return new Promise((resolve) => {
        const input = document.createElement('input');
        input.type = 'file';
        input.accept = '.png,image/png';
        let settled = false;
        const settle = (value) => {
            if (!settled) {
                settled = true;
                resolve(value);
            }
            input.remove();
        };
        input.addEventListener('change', () => {
            settle(input.files && input.files.length > 0 ? input.files[0] : null);
        });
        // 用户关掉选择器不会触发 change，焦点回到窗口时兜底收口。
        window.addEventListener('focus', () => setTimeout(() => settle(null), 300), {
            once: true,
        });
        input.click();
    });
}

// 把 PNG 文件读成 base64（去掉 data: 前缀）；失败返回空串。
function readPngAsBase64(file) {
    return new Promise((resolve) => {
        const reader = new FileReader();
        reader.addEventListener('error', () => resolve(''));
        reader.addEventListener('load', () => {
            const result = String(reader.result || '');
            const comma = result.indexOf(',');
            resolve(comma >= 0 ? result.slice(comma + 1) : result);
        });
        reader.readAsDataURL(file);
    });
}

// 接线：Change Skin 按钮 + 披风 pills 的点击（事件委托到 pill-row）+ Apply Cape Changes。
function setupSkinCapeControls() {
    const btn = document.getElementById('change-skin-btn');
    if (btn) {
        btn.addEventListener('click', () => {
            void handleChangeSkinClick();
        });
    }
    const capeRow = document.getElementById('cape-options');
    if (capeRow) {
        capeRow.addEventListener('click', (event) => {
            const pill = event.target.closest('.cape-pill');
            if (pill) {
                void handleCapePillClick(pill);
            }
        });
    }
    const applyBtn = document.getElementById('apply-cape-btn');
    if (applyBtn) {
        applyBtn.addEventListener('click', () => {
            void handleApplyCapeClick();
        });
    }
}

// 把已缓存的头像填进列表项（跳过没有头像缓存的账号）。
function applyListAvatars() {
    document.querySelectorAll('#accounts-list .account-item').forEach((item) => {
        const accountId = Number(item.dataset.accountId);
        const head = listAvatarCache.get(accountId);
        const img = item.querySelector('img[slot="start"]');
        if (img && head) {
            img.src = `data:image/png;base64,${head}`;
        }
    });
}

// 逐个请求列表里还缺头像的账号（磁盘命中时几乎即时）。
async function loadListAvatars() {
    const items = document.querySelectorAll('#accounts-list .account-item');
    for (const item of items) {
        const accountId = Number(item.dataset.accountId);
        if (!listAvatarCache.has(accountId)) {
            try {
                const skin = await invokeFn('get_account_skin', { accountId });
                if (skin.head_base64) {
                    listAvatarCache.set(accountId, skin.head_base64);
                }
            } catch (error) {
                console.error('Failed to load avatar', error);
            }
        }
        // 请求可能耗时，条目可能已被重渲染移除
        const img = item.querySelector('img[slot="start"]');
        const head = listAvatarCache.get(accountId);
        if (img && img.isConnected && head) {
            img.src = `data:image/png;base64,${head}`;
        }
    }
}

// ── Add Account dialog ────────────────────────────────────────────────
// The confirm button has no inline onclick: submission is fully handled in
// JS. On success the dialog is closed by JS only; on error it stays open.

// Ensures the shared status/error line inside the dialog content.
function ensureAddAccountMessage(dialog) {
    let el = dialog.querySelector('.add-account-message');
    if (!el) {
        el = document.createElement('p');
        el.className = 'add-account-message';
        el.style.cssText = 'margin:8px 0 0;font-size:0.85rem;line-height:1.5;';
        el.setAttribute('role', 'alert');
        el.hidden = true;
        const content = dialog.querySelector('[slot="content"]');
        (content || dialog).appendChild(el);
    }
    return el;
}

function showAddAccountError(message) {
    const dialog = document.getElementById('add-account-dialog');
    if (!dialog) {
        return;
    }
    const el = ensureAddAccountMessage(dialog);
    el.textContent = message;
    el.style.color = 'var(--md-sys-color-error)';
    el.hidden = !message;
}

// Busy state: disable both action buttons and show a waiting message.
function setAddAccountBusy(busy, message = '') {
    addAccountBusy = busy;
    const dialog = document.getElementById('add-account-dialog');
    if (!dialog) {
        return;
    }
    const confirmBtn = dialog.querySelector('[slot="confirm"]');
    const cancelBtn = dialog.querySelector('[slot="cancel"]');
    if (confirmBtn) {
        confirmBtn.disabled = busy;
    }
    if (cancelBtn) {
        cancelBtn.disabled = busy;
    }
    const el = ensureAddAccountMessage(dialog);
    if (busy) {
        el.textContent = message;
        el.style.color = 'var(--md-sys-color-on-surface-variant)';
        el.hidden = !message;
    }
}

// ── Microsoft login progress ──────────────────────────────────────────
// Rust 为登录链路的每一步广播 ms-login-progress 事件（打开浏览器 →
// 等待回调 → 换 MSA token → Xbox Live → XSTS → Minecraft 服务 → 玩家
// 资料 → 保存），payload 为 { step, total, label }，渲染成微软面板里
// 的 Material 3 线性进度条。

function msProgressEls() {
    const wrap = document.getElementById('ms-login-progress');
    const bar = document.getElementById('ms-login-bar');
    const step = document.getElementById('ms-login-step');
    return wrap && bar && step ? { wrap, bar, step } : null;
}

// 登录开始时显示进度块并复位进度条。
function showMsLoginProgress() {
    const els = msProgressEls();
    if (!els) {
        return;
    }
    els.bar.indeterminate = false;
    els.bar.value = 0;
    els.step.textContent = '准备打开系统浏览器…';
    els.wrap.hidden = false;
}

function hideMsLoginProgress() {
    const els = msProgressEls();
    if (els) {
        els.wrap.hidden = true;
    }
}

// 应用一条进度事件。Tauri 2 已把 JSON payload 反序列化成对象；字符串
// 分支只是防御性兜底。
function applyMsLoginProgress(payload) {
    const els = msProgressEls();
    if (!els) {
        return;
    }
    let data = payload;
    if (typeof data === 'string') {
        try {
            data = JSON.parse(data);
        } catch {
            return;
        }
    }
    if (!data || typeof data !== 'object') {
        return;
    }
    const { step, total, label } = data;
    if (typeof step === 'number' && typeof total === 'number' && total > 0) {
        els.bar.indeterminate = false;
        els.bar.value = Math.min(step / total, 1);
    }
    if (typeof label === 'string' && label) {
        els.step.textContent = label;
    }
}

// 在 invoke 登录命令之前订阅，避免漏掉最早的事件；返回清理函数。
// iframe 内由顶层壳（index.js）订阅并转发；页面独立打开时直接订阅。
async function subscribeMsLoginProgress() {
    return onPageEvent('ms-login-progress', (payload) => {
        applyMsLoginProgress(payload);
    });
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
    if (addAccountBusy) {
        return; // A login is in progress — do not disturb it.
    }
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
        name.removeAttribute('error-text');
    }
    hideMsLoginProgress();
    showAddAccountError('');
}

// Confirm-button handler: runs the matching Tauri command and closes the
// dialog on success. Errors keep the dialog open and show the message.
async function handleConfirm(dialog) {
    if (addAccountBusy) {
        return;
    }
    showAddAccountError('');

    try {
        let account = null;

        if (addAccountMode === ACCOUNT_MODE_MS) {
            // Microsoft: Rust opens the system browser and resolves through
            // the glow-squid://ms-auth deeplink. No arguments; this may take
            // a while (up to the 5-minute login timeout in Rust). Rust emits
            // ms-login-progress for every step, rendered as the progress bar
            // in the panel (the shared busy line stays empty on purpose —
            // the step label in the progress block carries the status).
            setAddAccountBusy(true);
            showMsLoginProgress();
            // 订阅必须在 invoke 之前完成，最早的进度事件才不会丢失。
            const unlisten = await subscribeMsLoginProgress();
            try {
                account = await invokeFn('login_microsoft');
            } finally {
                unlisten();
            }
        } else {
            // Offline: validate the player name, then invoke login_offline.
            const field = document.getElementById('offline-name');
            const name = field ? field.value.trim() : '';
            if (field) {
                field.toggleAttribute('error', !name);
                if (!name) {
                    field.setAttribute('error-text', '用户名不能为空');
                }
            }
            if (!name) {
                showAddAccountError('用户名不能为空');
                return;
            }
            setAddAccountBusy(true);
            account = await invokeFn('login_offline', {
                name: name.slice(0, MAX_PLAYER_NAME),
            });
        }

        dialog.close();
        await refreshAccounts();
        if (account && account.id != null) {
            selectedAccountId = account.id;
            markSelectedAccount();
            renderPreview();
        }
    } catch (error) {
        // e.g. “用户名不能为空”, “微软登录失败: …”, “登录已取消”,
        // “登录超时（5 分钟）”, “打开浏览器失败: …”
        // 注意：错误只在 catch 里显示，绝不能在 finally 里清空消息，
        // 否则会把刚显示的错误立刻抹掉（之前登录失败无提示即此因）。
        showAddAccountError(String(error));
    } finally {
        setAddAccountBusy(false);
    }
}

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

    // Never close while a login is in progress (deeplink callback pending).
    // Escape / backdrop both dispatch a cancelable `cancel` event.
    dialog.addEventListener('cancel', (event) => {
        if (addAccountBusy) {
            event.preventDefault();
        }
    });

    // Confirm button (slot="confirm") does not close the dialog by itself —
    // JS closes it on success only.
    const confirmBtn = dialog.querySelector('[slot="confirm"]');
    if (confirmBtn) {
        confirmBtn.addEventListener('click', () => {
            void handleConfirm(dialog);
        });
    }
}

// ── Remove Account menu (删除账号) ────────────────────────────────────
// delete_account 会级联清理：accounts 行、微软账号的 keyring 凭据、
// 皮肤缓存目录 skin/{microsoft|offline}/<uuid>/。点 Remove account 弹出
// md-menu，点「确认删除」菜单项执行删除（点击菜单项后菜单自动关闭），
// 点击菜单外区域即取消，不再弹 md-dialog。

let removeAccountBusy = false;

function setupRemoveAccount() {
    const menu = document.getElementById('remove-account-menu');
    const removeBtn = document.getElementById('remove-account-btn');
    const confirmItem = document.getElementById('confirm-remove-account-btn');
    if (!menu || !removeBtn || !confirmItem) {
        return;
    }

    // 点 Remove account 弹出菜单（无账号或删除进行中时忽略）。
    removeBtn.addEventListener('click', () => {
        const account = accounts.find((a) => a.id === selectedAccountId) || null;
        if (!account || removeAccountBusy) {
            return;
        }
        menu.open = true;
    });

    // 点「确认删除」菜单项执行删除；菜单项点击后菜单自动关闭。
    confirmItem.addEventListener('click', async () => {
        const account = accounts.find((a) => a.id === selectedAccountId) || null;
        if (!account || removeAccountBusy) {
            return;
        }
        removeAccountBusy = true;
        confirmItem.disabled = true;
        try {
            await invokeFn('delete_account', { accountId: account.id });
            listAvatarCache.delete(account.id);
            await refreshAccounts();
        } catch (error) {
            console.error('Failed to delete account', error);
        } finally {
            removeAccountBusy = false;
            confirmItem.disabled = false;
        }
    });
}

// ── Start Minecraft Menu ──────────────────────────────────────────────
// 启动版本下拉框：get_local_versions 填充 md-select-option；图标优先按
// loader（fabric/forge/neoforge），原版按 type（release/snapshot/old_*），
// 未知类型回退唱片图标。注意：后端 MinecraftVersion 的 JSON 键是 "type"
// （serde rename），不是 version_type。
const verIcon = [null, fabricImg, forgeImg, neoforgeImg];   // 下标 = loader
const verIcon2 = [releaseImg, snapshotImg, oldImg, oldImg]; // 下标 = type 0-3

function versionIcon(ver) {
    return verIcon[ver.loader] || verIcon2[ver.type] || musicDiscImg;
}

async function setupLaunchMenu() {
    const LaunchVerSelect = document.getElementById('launch-ver-select');
    const LaunchBtn = document.getElementById('launch-btn');
    if (!LaunchVerSelect || !LaunchBtn) {
        return;
    }

    let LocalVersions = [];
    try {
        LocalVersions = await invokeFn('get_local_versions');
    } catch (error) {
        console.error('Failed to load local versions', error);
    }

    // 一次性写入，避免逐条 += 反复重解析。
    LaunchVerSelect.innerHTML = LocalVersions.map((ver) => `
        <md-select-option value="${escapeHtml(ver.name)}">
            <img slot="start" src="${versionIcon(ver)}" class="icon" alt="" />
            <span slot="headline">${escapeHtml(ver.name)}</span>
        </md-select-option>
    `).join('');

    // select 字段左侧显示当前选中版本的图标（md-select 原生支持
    // slot="leading-icon"；关闭状态的字段不会渲染选项里的 slot="start"
    // 图标，所以图标要单独同步，选项打开后由菜单自身渲染）。
    const LaunchVerIcon = document.createElement('img');
    LaunchVerIcon.slot = 'leading-icon';
    LaunchVerIcon.alt = '';
    LaunchVerIcon.hidden = true;
    LaunchVerSelect.appendChild(LaunchVerIcon);
    const syncLaunchIcon = () => {
        const cur = LocalVersions.find((ver) => ver.name === LaunchVerSelect.value);
        LaunchVerIcon.hidden = !cur;
        if (cur) {
            LaunchVerIcon.src = versionIcon(cur);
        }
    };

    // 默认选中第一个版本；没有任何版本时禁用 Start。
    // 注意：md-select 关闭态字段的文本来自 displayText，而选项的 displayText
    // 是实时查询 headline slot 得到的。选项刚写入 DOM 时还没完成首次渲染
    // (renderRoot 为 undefined，查询返回空数组)，此时立即赋值 value 会把
    // displayText 固定为空 —— 图标正常但字段不显示版本名。所以必须先等
    // 所有选项渲染完成，再赋值并校验。
    const options = [...LaunchVerSelect.querySelectorAll('md-select-option')];
    await Promise.all(options.map((opt) => opt.updateComplete));
    const first = LocalVersions[0]?.name ?? null;
    if (first) {
        LaunchVerSelect.value = first;
        await LaunchVerSelect.updateComplete;
        if (LaunchVerSelect.value !== first || !LaunchVerSelect.displayText) {
            // 兜底：重设 value 重新走 selectItem → updateValueAndDisplayText，
            // displayText 此时会实时读到已渲染的 headline 文本。
            LaunchVerSelect.value = first;
            await LaunchVerSelect.updateComplete;
        }
    }
    LaunchBtn.disabled = !first;
    syncLaunchIcon();

    LaunchVerSelect.addEventListener('change', () => {
        LaunchBtn.disabled = !LaunchVerSelect.value;
        syncLaunchIcon();
    });

    // TODO: 启动逻辑待后端提供 launch 命令后接入。
    LaunchBtn.addEventListener('click', () => {
        if (LaunchVerSelect.value) {
            console.warn('launch command not implemented yet:', LaunchVerSelect.value);
        }
    });
}
// ── Init ──────────────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', async () => {
    if (!isPage('home.html')) {
        return;
    }
    setupAccountList();
    setupAddAccountDialog();
    setupRemoveAccount();
    setupSkinCapeControls();
    void setupLaunchMenu();
    await refreshAccounts();
});
