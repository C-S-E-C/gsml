// ── Settings page module ───────────────────────────────────────────────
// settings.html loads this module. It owns the settings sidebar (category
// list on the left) and the content pane on the right. Only the "About"
// view exists for now; more views can be added to the list / #view-* panels.

import { isPage } from './main.js';

const VIEWS = [
    { key: 'about' },
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

function initSettingsPage() {
    // Default to the first (only) view: "about".
    const first = VIEWS.length > 0 ? VIEWS[0].key : 'about';
    setActiveView(first);
}

document.addEventListener('DOMContentLoaded', async () => {
    if (!isPage('settings.html')) {
        return;
    }
    initSettingsPage();
});

// ── Global bridge ────────────────────────────────────────────────────
// settings.js runs as a module, so its top-level declarations are not
// visible to inline `onclick="..."` attributes in settings.html.
window.switchSettingsView = switchSettingsView;
