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
                name: name
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
const VER_ICON = [null, fabricImg, forgeImg, neoforgeImg];   // 下标 = loader
const VER_ICON2 = [releaseImg, snapshotImg, oldImg, oldImg]; // 下标 = type 0-3

function versionIcon(ver) {
    return VER_ICON[ver.loader] || VER_ICON2[ver.type] || musicDiscImg;
}

async function setupLaunchMenu() {
    const launchVerSelect = document.getElementById('launch-ver-select');
    const launchBtn = document.getElementById('launch-btn');
    if (!launchVerSelect || !launchBtn) {
        return;
    }

    let localVersions = [];
    try {
        localVersions = await invokeFn('get_local_versions');
    } catch (error) {
        console.error('Failed to load local versions', error);
    }

    // 一次性写入，避免逐条 += 反复重解析。
    launchVerSelect.innerHTML = localVersions.map((ver) => `
        <md-select-option value="${escapeHtml(ver.name)}">
            <img slot="start" src="${versionIcon(ver)}" class="icon" alt="" />
            <span slot="headline">${escapeHtml(ver.name)}</span>
        </md-select-option>
    `).join('');

    // select 字段左侧显示当前选中版本的图标（md-select 原生支持
    // slot="leading-icon"；关闭状态的字段不会渲染选项里的 slot="start"
    // 图标，所以图标要单独同步，选项打开后由菜单自身渲染）。
    const launchVerIcon = document.createElement('img');
    launchVerIcon.slot = 'leading-icon';
    launchVerIcon.alt = '';
    launchVerIcon.hidden = true;
    launchVerSelect.appendChild(launchVerIcon);
    const syncLaunchIcon = () => {
        const cur = localVersions.find((ver) => ver.name === launchVerSelect.value);
        launchVerIcon.hidden = !cur;
        if (cur) {
            launchVerIcon.src = versionIcon(cur);
        }
    };

    // 默认选中第一个版本；没有任何版本时禁用 Start。
    // 注意：md-select 关闭态字段的文本来自 displayText，而选项的 displayText
    // 是实时查询 headline slot 得到的。选项刚写入 DOM 时还没完成首次渲染
    // (renderRoot 为 undefined，查询返回空数组)，此时立即赋值 value 会把
    // displayText 固定为空 —— 图标正常但字段不显示版本名。所以必须先等
    // 所有选项渲染完成，再赋值并校验。
    const options = [...launchVerSelect.querySelectorAll('md-select-option')];
    await Promise.all(options.map((opt) => opt.updateComplete));
    const first = localVersions[0]?.name ?? null;
    if (first) {
        launchVerSelect.value = first;
        await launchVerSelect.updateComplete;
        if (launchVerSelect.value !== first || !launchVerSelect.displayText) {
            // 兜底：重设 value 重新走 selectItem → updateValueAndDisplayText，
            // displayText 此时会实时读到已渲染的 headline 文本。
            launchVerSelect.value = first;
            await launchVerSelect.updateComplete;
        }
    }
    setLaunchEnabled(!!first);
    syncLaunchIcon();

    launchVerSelect.addEventListener('change', () => {
        setLaunchEnabled(!!launchVerSelect.value);
        syncLaunchIcon();
    });

    // 启动 / 停止：运行中点击同一颗 FAB 即停止游戏。
    launchBtn.addEventListener('click', async () => {
        if (launchStatus.running) {
            // 停止只是一次后端调用；视觉状态由随后到达的 stopped 事件切换。
            // 这里不长期禁用按钮：stop_game 幂等，连点无副作用，
            // 也避免事件丢失时按钮永久禁用。
            setLaunchEnabled(false);
            try {
                await invokeFn('stop_game');
            } catch (error) {
                console.error('stop game failed', error);
            } finally {
                setLaunchEnabled(true);
            }
            return;
        }
        if (launchBtn.getAttribute('aria-disabled') === 'true') {
            return; // 键盘激活也走这里：无版本 / 启动中都不可重复触发
        }
        if (!launchVerSelect.value) {
            return;
        }
        if (selectedAccountId == null) {
            return;
        }
        // 打开进度 dialog（非日志），进入运行态前先置为不可点。
        launchRunning(false);
        setLaunchEnabled(false);
        openLaunchProgress();
        const versionId = launchVerSelect.value;
        try {
            // Java/内存优先级：版本覆盖 > 设置页全局 java-path > 后端按版本
            // JSON 的 javaVersion 自动挑选；内存未设置则用后端默认值。
            const [perVersion, globalJava] = await Promise.all([
                readVersionSettings(versionId),
                invokeFn('get_svar', { key: 'java-path' }).catch(() => null),
            ]);
            await invokeFn('launch_game', {
                versionId,
                accountId: selectedAccountId,
                javaPath: perVersion.javaPath
                    || (typeof globalJava === 'string' ? globalJava : null),
                memoryMb: perVersion.memoryMb || undefined,
            });
        } catch (error) {
            // 不做 close→show：md-dialog 关闭动画进行中立刻 show() 会被收尾的
            // 关闭动画覆盖，报错弹窗会一闪而过。dialog 已打开时原地切换为错误模式。
            launchRunning(false);
            showLaunchError('启动失败', error);
            console.error('launch failed', error);
        }
    });

    // 启动状态事件（launch-status）。启动完成后（running，即 Minecraft 有窗口
    // 后）关闭进度 dialog，并启用「停止游戏」按钮；game-output 只进环形缓冲，
    // 供报错时展示最近日志。
    onPageEvent('launch-status', (payload) => {
        const { status } = payload;
        if (status === 'starting') {
            gameOutputBuffer.length = 0;
            if (payload.detail) {
                launchStatus.lastText = payload.detail;
            }
            openLaunchProgress();
        } else if (status === 'running') {
            // 启动完成：关闭进度 dialog，FAB 切为 Stop 态（点击即停止游戏）。
            closeLaunchProgress();
            launchRunning(true);
        } else if (status === 'stopped') {
            closeLaunchProgress();
            launchRunning(false);
        } else if (status === 'error') {
            // 同上：原地切换为错误模式，不做 close→show（关闭动画竞态会让
            // 报错弹窗一闪而过）。
            launchRunning(false);
            showLaunchError(payload.stage === 'crash' ? '游戏异常退出' : '启动失败', payload);
        }
    });

    onPageEvent('game-output', (payload) => {
        if (typeof payload?.line !== 'string' || !payload.line) return;
        gameOutputBuffer.push(payload.line);
        if (gameOutputBuffer.length > GAME_OUTPUT_LIMIT) {
            gameOutputBuffer.splice(0, gameOutputBuffer.length - GAME_OUTPUT_LIMIT);
        }
    });
}
// ── 启动状态 ────────────────────────────────────────────────────────────
// 进展显示改用 md-dialog（home.html 中的 #launch-progress-dialog），
// 不再内嵌日志控制台。
const GAME_OUTPUT_LIMIT = 400;
const gameOutputBuffer = [];
const launchStatus = { running: false, statusEl: null, lastText: null };

// 打开/更新启动进度 dialog（进度模式：显示进度条 + 状态文字）
// 弹窗打开期间锁住页面滚动：否则背景滚动容器的滚动条会一直挂在边上，
// 与弹窗内的日志滚动条混在一起（视觉上像「弹窗里有多条滚动条」）。
function setPageScrollLocked(locked) {
    document.body.classList.toggle('dialog-open', locked);
}

function openLaunchProgress() {
    const dialog = document.getElementById('launch-progress-dialog');
    const statusEl = document.getElementById('launch-dialog-status');
    if (statusEl) {
        statusEl.textContent = launchStatus.lastText || '正在启动…';
    }
    setDialogMode(false); // 进度模式
    setPageScrollLocked(true);
    if (dialog && !dialog.open) {
        void dialog.show();
    }
}

// 切换到错误模式：隐藏进度条，展示错误信息 + 「关闭」按钮（需手动确认）。
// payload 兼容两种形态：结构化 launch-status 载荷
// { detail, stage?, code?, aliveSecs?, pid?, tail? } 或纯字符串（invoke reject）。
const LAUNCH_STAGE_LABELS = {
    resolve: '解析账号/配置',
    launch: '启动进程',
    crash: '进程崩溃',
};

function formatLaunchErrorText(summary, meta, logLines) {
    const parts = [summary];
    if (meta) {
        parts.push(meta.map(([k, v]) => `${k}: ${v}`).join('，'));
    }
    if (logLines.length > 0) {
        parts.push('--- 最近日志 ---', ...logLines);
    }
    return parts.join('\n');
}

function showLaunchError(title, payload) {
    // 仅当载荷带有结构化字段（detail/stage）时按结构化处理；
    // 纯字符串（invoke reject 的 Err 文本）或 Error 对象直接取文案。
    const isStructured = !!payload
        && typeof payload === 'object'
        && ('detail' in payload || 'stage' in payload);
    const summary = isStructured
        ? (payload.detail || '未知错误')
        : (payload instanceof Error ? payload.message : String(payload ?? '未知错误'));
    const stage = isStructured ? payload.stage : null;
    const code = isStructured ? payload.code : null;
    const aliveSecs = isStructured ? payload.aliveSecs : null;
    const pid = isStructured ? payload.pid : null;
    const tail = isStructured && typeof payload.tail === 'string' ? payload.tail : '';

    setDialogMode(true);
    setPageScrollLocked(true);
    const titleEl = document.getElementById('launch-dialog-title');
    if (titleEl) {
        titleEl.textContent = title || '启动失败';
    }
    const summaryEl = document.getElementById('launch-error-summary');
    if (summaryEl) {
        summaryEl.textContent = summary;
    }

    // 元信息行：只显示有值的项。
    const meta = [];
    if (code !== null && code !== undefined) {
        meta.push(['退出码', code]);
    }
    if (aliveSecs !== null && aliveSecs !== undefined) {
        meta.push(['存活', `${aliveSecs}s`]);
    }
    if (pid !== null && pid !== undefined) {
        meta.push(['PID', pid]);
    }
    if (stage && LAUNCH_STAGE_LABELS[stage]) {
        meta.push(['阶段', LAUNCH_STAGE_LABELS[stage]]);
    }
    const metaEl = document.getElementById('launch-error-meta');
    if (metaEl) {
        metaEl.hidden = meta.length === 0;
        metaEl.innerHTML = meta
            .map(([k, v]) => `<span class="launch-error-chip">${escapeHtml(String(k))}：${escapeHtml(String(v))}</span>`)
            .join('');
    }

    // 日志区：优先实时缓冲（崩溃前的最后输出），其次后端附带的 tail。
    const tailLines = tail ? tail.split(/\r?\n/).filter((l) => l.trim()) : [];
    const logLines = gameOutputBuffer.length > 0 ? gameOutputBuffer : tailLines;
    const logEl = document.getElementById('launch-error-log');
    if (logEl) {
        logEl.hidden = logLines.length === 0;
        logEl.textContent = logLines.join('\n');
    }

    // 日志/摘要提到 Java 时提供直达设置的入口。
    const javaRelated = /java/i.test(`${summary}\n${logLines.join('\n')}`);
    const settingsBtn = document.getElementById('launch-error-settings');
    if (settingsBtn) {
        settingsBtn.hidden = !javaRelated;
    }

    // 复制按钮：复制摘要 + 元信息 + 日志全文。
    const copyBtn = document.getElementById('launch-error-copy');
    if (copyBtn) {
        copyBtn.replaceWith(copyBtn.cloneNode(true));
        const freshCopy = document.getElementById('launch-error-copy');
        freshCopy.addEventListener('click', async () => {
            const fullText = formatLaunchErrorText(summary, meta, logLines);
            try {
                await navigator.clipboard.writeText(fullText);
                freshCopy.textContent = '已复制';
            } catch (err) {
                console.error('copy launch error failed:', err);
                freshCopy.textContent = '复制失败';
            }
            setTimeout(() => {
                freshCopy.textContent = '复制日志';
            }, 1500);
        });
    }

    const settingsBtn2 = document.getElementById('launch-error-settings');
    if (settingsBtn2) {
        settingsBtn2.onclick = () => {
            // 先隐藏错误内容，绕过 closeLaunchProgress 的「报错展示中不关闭」保护。
            const errorEl = document.getElementById('launch-dialog-error');
            if (errorEl) errorEl.hidden = true;
            closeLaunchProgress();
            // 设置页是顶层 shell 的一个 tab；同源 iframe 里调用顶层导航。
            try {
                window.parent.sidebar_button('settings');
            } catch (err) {
                console.error('navigate to settings failed:', err);
            }
        };
    }

    const dialog = document.getElementById('launch-progress-dialog');

    // 关闭按钮：错误弹窗的显式关闭入口。它嵌在 <div slot="actions"> 里，
    // Material 的 cancel 槽位内置关闭在这里并不可靠，故直接接事件强制收尾：
    // 先隐藏错误内容绕过 closeLaunchProgress 的「报错展示中不自动关闭」保护，
    // 再关闭弹窗（与「前往设置」按钮同一套收尾路径）。
    const closeBtn = document.getElementById('launch-dialog-close');
    if (closeBtn) {
        closeBtn.onclick = () => {
            const errorEl = document.getElementById('launch-dialog-error');
            if (errorEl) errorEl.hidden = true;
            if (dialog?.open) void dialog.close();
            setPageScrollLocked(false);
        };
    }

    if (dialog && !dialog.open) {
        void dialog.show();
    }
}

// mode: false=进度模式（进度条显示、错误区隐藏、无关闭按钮）
//      true =错误模式（进度条隐藏、错误区显示、有关闭按钮）
function setDialogMode(errorMode) {
    const bar = document.getElementById('launch-dialog-bar');
    if (bar) {
        bar.hidden = errorMode;
    }
    const errorEl = document.getElementById('launch-dialog-error');
    if (errorEl) {
        errorEl.hidden = !errorMode;
    }
    const close = document.getElementById('launch-dialog-close');
    if (close) {
        close.hidden = !errorMode;
    }
    const statusEl = document.getElementById('launch-dialog-status');
    if (statusEl) {
        statusEl.hidden = errorMode;
    }
}

function closeLaunchProgress() {
    const dialog = document.getElementById('launch-progress-dialog');
    // 报错信息展示中：不自动关闭（等用户点「关闭」），避免后续
    // stopped/running 事件把错误弹窗冲掉。
    const errorEl = document.getElementById('launch-dialog-error');
    if (errorEl && !errorEl.hidden && dialog?.open) {
        return;
    }
    if (dialog?.open) {
        void dialog.close();
    }
    setPageScrollLocked(false);
}

// md-branded-fab 并不支持 disabled（组件没有该属性，赋值只会落在自定义
// 属性上、毫无效果），因此用 aria-disabled + CSS 表达「当前不可点」，
// 点击/键盘激活处也显式判一次，避免启动中重复 invoke。
function setLaunchEnabled(enabled) {
    const btn = document.getElementById('launch-btn');
    if (!btn) return;
    if (enabled) {
        btn.removeAttribute('aria-disabled');
    } else {
        btn.setAttribute('aria-disabled', 'true');
    }
}

function launchRunning(running) {
    launchStatus.running = running;
    const btn = document.getElementById('launch-btn');
    if (btn) {
        // 运行中不禁用：同一颗 FAB 变成「停止游戏」，点击即调用 stop_game；
        // 空闲态维持原规则——没选版本就不可点。
        setLaunchEnabled(running || !!document.getElementById('launch-ver-select')?.value);
        btn.label = running ? 'Stop' : 'Launch';
        btn.setAttribute('aria-label', running ? 'Stop game' : 'Launch game');
        btn.toggleAttribute('running', running);
        const icon = btn.querySelector('md-icon') || null;
        if (icon) {
            icon.textContent = running ? 'stop_circle' : 'play_arrow';
        }
    }
    // 运行中不允许切换版本，避免下拉框与已启动的实例状态不一致。
    const select = document.getElementById('launch-ver-select');
    if (select) {
        select.disabled = running;
    }
}

// ── Version settings（单版本 Java / 内存覆盖）────────────────────────────
// 覆盖值按版本分别存 svar：java-path:<版本> / memory-mb:<版本>。
// 未设置的项回落到设置页的全局 java-path 与后端默认内存（1024 MB）。
const versionJavaKey = (version) => `java-path:${version}`;
const versionMemoryKey = (version) => `memory-mb:${version}`;

let javaRuntimesCache = null;

async function loadJavaRuntimes() {
    if (javaRuntimesCache) {
        return javaRuntimesCache;
    }
    try {
        const list = await invokeFn('list_java_installations');
        const runtimes = Array.isArray(list) ? list : [];
        // 空结果不缓存：可能是「游戏运行中被跳过」或临时扫描失败，
        // 下次打开对话框应重新扫描。
        if (runtimes.length > 0) {
            javaRuntimesCache = runtimes;
        }
        return runtimes;
    } catch (error) {
        console.error('list java installations failed', error);
        return [];
    }
}

/** 读取某版本的覆盖配置（未设置/非法值为 null）。 */
async function readVersionSettings(version) {
    const [java, memory] = await Promise.all([
        invokeFn('get_svar', { key: versionJavaKey(version) }).catch(() => null),
        invokeFn('get_svar', { key: versionMemoryKey(version) }).catch(() => null),
    ]);
    return {
        javaPath: typeof java === 'string' && java.trim() ? java.trim() : null,
        memoryMb: typeof memory === 'number' && memory >= 512 ? memory : null,
    };
}

function setupVersionSettings() {
    const dialog = document.getElementById('version-settings-dialog');
    const gear = document.getElementById('version-settings-btn');
    const saveBtn = document.getElementById('version-settings-save');
    const cancelBtn = document.getElementById('version-settings-cancel');
    const javaSelect = document.getElementById('version-settings-java');
    const memoryInput = document.getElementById('version-settings-memory');
    const nameEl = document.getElementById('version-settings-name');
    if (!dialog || !gear || !saveBtn || !javaSelect || !memoryInput) {
        return;
    }

    let editingVersion = '';

    cancelBtn?.addEventListener('click', () => {
        if (dialog.open) {
            void dialog.close();
        }
    });

    gear.addEventListener('click', async () => {
        const versionSelect = document.getElementById('launch-ver-select');
        const version = versionSelect?.value || '';
        if (!version) {
            return;
        }
        editingVersion = version;
        if (nameEl) {
            nameEl.textContent = version;
        }

        // Java 候选：Auto + 本机扫描结果（Auto 交给后端按 javaVersion 挑）。
        const runtimes = await loadJavaRuntimes();
        const hintEl = document.getElementById('version-settings-hint');
        if (hintEl) {
            if (runtimes.length > 0) {
                hintEl.textContent = '留空的项目沿用全局设置（Settings → Java）与后端默认值。';
            } else {
                const running = await invokeFn('is_game_running').catch(() => false);
                hintEl.textContent = running
                    ? '游戏运行中，已跳过 Java 扫描；停止游戏后重新打开本对话框可看到完整列表。'
                    : '未检测到已安装的 Java；Auto 会由后端按版本要求挑选。';
            }
        }
        javaSelect.innerHTML = [
            `<md-select-option value="auto">
                <span slot="headline">Auto — 按版本要求自动选择</span>
            </md-select-option>`,
            ...runtimes.map((rt) => `
                <md-select-option value="${escapeHtml(rt.path)}">
                    <span slot="headline">${escapeHtml(
                        `${rt.version || '?'} · ${rt.vendor || 'Java'}${rt.kind === 'JDK' ? '' : ' (JRE)'}`,
                    )}</span>
                </md-select-option>`),
        ].join('');
        // 选项完成首渲染后赋 value，否则关闭态 displayText 会是空（见 setupLaunchMenu 注释）。
        await Promise.all(
            [...javaSelect.querySelectorAll('md-select-option')].map((opt) => opt.updateComplete),
        );

        const current = await readVersionSettings(version);
        const wanted = current.javaPath || 'auto';
        javaSelect.value = wanted;
        await javaSelect.updateComplete;
        if (javaSelect.value !== wanted) {
            javaSelect.value = wanted;
            await javaSelect.updateComplete;
        }
        memoryInput.value = current.memoryMb ? String(current.memoryMb) : '';

        if (!dialog.open) {
            void dialog.show();
        }
    });

    saveBtn.addEventListener('click', async () => {
        if (!editingVersion) {
            return;
        }
        saveBtn.disabled = true;
        try {
            const pick = javaSelect.value;
            const javaPath = !pick || pick === 'auto' ? null : pick;
            const parsed = Number.parseInt(memoryInput.value, 10);
            const memoryMb = Number.isFinite(parsed) && parsed >= 512 ? parsed : null;
            await invokeFn('set_svar', { key: versionJavaKey(editingVersion), value: javaPath });
            await invokeFn('set_svar', { key: versionMemoryKey(editingVersion), value: memoryMb });
            if (dialog.open) {
                void dialog.close();
            }
        } catch (error) {
            console.error('save version settings failed', error);
        } finally {
            saveBtn.disabled = false;
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
    setupVersionSettings();
    // md-dialog 可能被 Esc / 点击遮罩关闭，确保滚动锁总能解开
    document
        .getElementById('launch-progress-dialog')
        ?.addEventListener('close', () => setPageScrollLocked(false));
    void setupLaunchMenu();
    await refreshAccounts();
});
