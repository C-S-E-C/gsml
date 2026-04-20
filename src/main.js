const app = {
    channel: 'ALPHA',
    version: '1.0.0',
};

const invoke = window.__TAURI_INTERNALS__.invoke;
const MAX_PLAYER_NAME_LENGTH = 16;
const thesquidsays = [
    "Ayo, stop touching me!",
    "I'm not a toy!",
    "Leave me alone!",
    "Don't you understand?",
    "Hell nah!"
];

console.log(`[ALPHA] ${app.version}`);
console.log(`[PATH] ${location.pathname}`);

function isRootPage() {
    return location.pathname === '/' || location.pathname.endsWith('/index.html');
}

function isPage(name) {
    return location.pathname === `/${name}` || location.pathname.endsWith(`/${name}`);
}

function escapeHtml(text) {
    return String(text).replace(/[&<>"']/g, (c) => ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;'
    }[c]));
}

function sidebar_button(name, btn) {
    document.querySelectorAll('.sidebar button').forEach((b) => b.classList.remove('active'));
    if (btn) {
        btn.classList.add('active');
    }
    const frame = document.getElementById('main');
    if (frame) {
        frame.src = `components/${name}.html`;
    }
}

function requiredjava(date) {
    date = new Date(date);
    if (date < new Date('2021-06-08')) {
        return 8;
    }
    if (date < new Date('2021-11-30')) {
        return 16;
    }
    return 17;
}

var logoclickedtimes = 0;
function logoclick() {
    if (logoclickedtimes == thesquidsays.length) {
        document.getElementById("logo").src = "assets/him.png";
        document.getElementById("js-css").innerHTML += `:root {
           --root-bgcolor: #320000;
           --text-color: #f00;
           --bg-color: #4d0000;
        }`;
        alert("YOU PISSED ME OFF!");
        logoclickedtimes = 0;
    } else {
        alert("The squid:\n" + thesquidsays[logoclickedtimes]);
    }
    logoclickedtimes++;
}

async function ensureLauncherState() {
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

async function refreshInstalledVersions() {
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

function getVersionIcon(version) {
    const lowerVersion = version.toLowerCase();
    let imgsrc = "../assets/";
    if (lowerVersion.includes("fabric")) {
        imgsrc += "fabric.png";
    } else if (lowerVersion.includes("neoforge")) {
        imgsrc += "neoforge.png";
    } else if (lowerVersion.includes("forge")) {
        imgsrc += "anvil.png";
    } else if (/^1\.\d+/.test(version)) {
        imgsrc += "grass_block.png";
    } else if (lowerVersion.includes("w") || lowerVersion.includes("snapshot")) {
        imgsrc += "dirt.png";
    } else if (lowerVersion.startsWith("rd") || lowerVersion.startsWith("a") || lowerVersion.startsWith("b") || lowerVersion.startsWith("c") || lowerVersion.startsWith("inf")) {
        imgsrc += "cobblestone.png";
    } else {
        imgsrc += "music_disc_11.png";
    }
    return imgsrc;
}

function initVersionsPage() {
    const list = document.getElementById('version-list');
    if (!list) {
        return;
    }

    const versions = JSON.parse(localStorage.getItem('MC_VERSIONS') || '[]');
    const selectedVersion = localStorage.getItem('MC_SELECTED_VERSION');

    list.innerHTML = versions.map((version) => `
        <li class="version ${selectedVersion === version ? 'selected' : ''}" data-version="${escapeHtml(version)}">
            <img src="${getVersionIcon(version)}" alt="${escapeHtml(version)}" />
            <h3>${escapeHtml(version)}</h3>
        </li>
    `).join('');

    list.querySelectorAll('.version').forEach((item) => {
        item.addEventListener('click', () => {
            const version = item.dataset.version;
            if (!version) {
                return;
            }
            localStorage.setItem('MC_SELECTED_VERSION', version);
            initVersionsPage();
        });
    });
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

    mcPathText.textContent = localStorage.getItem('MC_PATH') || 'Not set';
    nameInput.value = localStorage.getItem('PLAYER_NAME') || 'Player';
    renderVersionOptions();

    nameInput.addEventListener('input', () => {
        const name = nameInput.value.trim().slice(0, MAX_PLAYER_NAME_LENGTH);
        localStorage.setItem('PLAYER_NAME', name || 'Player');
    });

    versionSelect.addEventListener('change', () => {
        localStorage.setItem('MC_SELECTED_VERSION', versionSelect.value);
        status.textContent = '';
    });

    refreshButton.addEventListener('click', async () => {
        status.textContent = 'Refreshing versions...';
        await refreshInstalledVersions();
        renderVersionOptions();
        status.textContent = 'Versions refreshed.';
    });

    startButton.addEventListener('click', async () => {
        const mcPath = localStorage.getItem('MC_PATH') || '';
        const version = versionSelect.value;
        const playerName = (nameInput.value || 'Player').trim().slice(0, MAX_PLAYER_NAME_LENGTH) || 'Player';

        if (!version) {
            status.textContent = 'Please pick a version first.';
            return;
        }

        startButton.disabled = true;
        status.textContent = 'Starting Minecraft...';
        try {
            const result = await invoke('start_minecraft', { mcPath, version, playerName });
            status.textContent = result;
        } catch (error) {
            status.textContent = `Failed to start Minecraft: ${error}`;
        } finally {
            startButton.disabled = false;
        }
    });
}

document.addEventListener('DOMContentLoaded', async () => {
    if (isRootPage()) {
        if (localStorage.getItem("StartTimes")) {
            await ensureLauncherState();
            await refreshInstalledVersions();
            document.getElementsByClassName("sidebar")[0].style.display = "flex";
            localStorage.setItem("StartTimes", Number(localStorage.getItem("StartTimes")) + 1);
            sidebar_button('home', document.querySelector('.sidebar button'));
        } else {
            location.pathname = '/user_license.html';
            return;
        }
    }

    if (isPage('components/home.html')) {
        await ensureLauncherState();
        await refreshInstalledVersions();
        await initHomePage();
    }

    if (isPage('components/versions.html')) {
        await refreshInstalledVersions();
        initVersionsPage();
    }
});
