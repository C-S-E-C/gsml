// ── Download / installed versions page module ────────────────────────
// download.html (root) loads this module. It renders the grid of installed
// Minecraft versions and remembers the selected one.

import { isPage, escapeHtml, refreshInstalledVersions } from './main.js';

// Version icons are bundled through Vite; imported so the build hashes them.
import fabricImg from '../assets/fabric.png';
import neoforgeImg from '../assets/neoforge.png';
import anvilImg from '../assets/anvil.png';
import grassBlockImg from '../assets/grass_block.png';
import dirtImg from '../assets/dirt.png';
import cobblestoneImg from '../assets/cobblestone.png';
import musicDiscImg from '../assets/music_disc_11.png';

function getVersionIcon(version) {
    const lowerVersion = version.toLowerCase();
    if (lowerVersion.includes("fabric")) {
        return fabricImg;
    } else if (lowerVersion.includes("neoforge")) {
        return neoforgeImg;
    } else if (lowerVersion.includes("forge")) {
        return anvilImg;
    } else if (/^1\.\d+/.test(version)) {
        return grassBlockImg;
    } else if (lowerVersion.includes("w") || lowerVersion.includes("snapshot")) {
        return dirtImg;
    } else if (lowerVersion.startsWith("rd") || lowerVersion.startsWith("a") || lowerVersion.startsWith("b") || lowerVersion.startsWith("c") || lowerVersion.startsWith("inf")) {
        return cobblestoneImg;
    }
    return musicDiscImg;
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

document.addEventListener('DOMContentLoaded', async () => {
    if (!isPage('download.html')) {
        return;
    }
    await refreshInstalledVersions();
    initVersionsPage();
});

// ── Global bridge ────────────────────────────────────────────────────
window.initVersionsPage = initVersionsPage;