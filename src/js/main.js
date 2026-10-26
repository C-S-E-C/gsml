// ── Shared core module ────────────────────────────────────────────────
// Public helpers used by every page. Page-specific logic lives in
// per-page modules loaded by each page:
//   · index.js     → launcher shell (tabs, window controls, logo)
//   · home.js      → Home page
//   · versions.js  → Download (installed versions) page
// main.js exposes these helpers via `export` for import by those modules.
// It performs no page bootstrapping of its own.

const app = {
    channel: 'ALPHA',
    version: '1.0.0',
};

const invoke = window.__TAURI_INTERNALS__.invoke;
const MAX_PLAYER_NAME_LENGTH = 16;

console.log(`[ALPHA] ${app.version}`);
console.log(`[PATH] ${location.pathname}`);

export const APP = app;
export const invokeFn = invoke;
export const MAX_PLAYER_NAME = MAX_PLAYER_NAME_LENGTH;

export function isRootPage() {
    return location.pathname === '/' || location.pathname.endsWith('/index.html');
}

export function isPage(name) {
    return location.pathname === `/${name}` || location.pathname.endsWith(`/${name}`);
}

export function escapeHtml(text) {
    return String(text).replace(/[&<>"']/g, (c) => ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;'
    }[c]));
}

export function requiredjava(date) {
    date = new Date(date);
    if (date < new Date('2021-06-08')) {
        return 8;
    }
    if (date < new Date('2021-11-30')) {
        return 16;
    }
    return 17;
}

export async function ensureLauncherState() {
    if (!localStorage.getItem('MC_PATH')) {
        try {
            const appData = await invoke('get_env_var', { name: 'APPDATA' });
            localStorage.setItem('MC_PATH', appData.replace(/\\/g, '/') + '/.minecraft');
        } catch (error) {
            console.error('Failed to get APPDATA path', error);
        }
    }

    if (!localStorage.getItem('PLAYER_NAME')) {
        localStorage.setItem('PLAYER_NAME', 'Player');
    }
}

export async function refreshInstalledVersions() {
    const mcPath = localStorage.getItem('MC_PATH');
    if (!mcPath) {
        localStorage.setItem('MC_VERSIONS', JSON.stringify([]));
        return [];
    }

    try {
        const versions = await invoke('list_minecraft_versions', { mcPath });
        localStorage.setItem('MC_VERSIONS', JSON.stringify(versions));

        const selected = localStorage.getItem('MC_SELECTED_VERSION');
        if (versions.length === 0) {
            localStorage.removeItem('MC_SELECTED_VERSION');
        } else if (!selected || !versions.includes(selected)) {
            localStorage.setItem('MC_SELECTED_VERSION', versions[0]);
        }
        return versions;
    } catch (error) {
        console.error('Failed to load versions', error);
        return JSON.parse(localStorage.getItem('MC_VERSIONS') || '[]');
    }
}