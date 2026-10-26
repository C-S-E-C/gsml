#!/usr/bin/env node
/* eslint-env node */
/* eslint-disable no-console */

/**
 * generate-licenses.mjs
 *
 * Build-time license generator used as `beforeBuildCommand`.
 *
 * Steps:
 *   1. Generate the Rust crate license page with cargo-about into
 *      public/THIRD_PARTY_LICENSES.html  (cwd = src-tauri).
 *   2. Query pnpm's dependency licenses with `pnpm licenses list --json`.
 *   3. Parse the JSON and insert a grouped pnpm section at the end of the
 *      SAME page, just before the closing `</main>`.
 *
 * The resulting public/THIRD_PARTY_LICENSES.html is copied by Vite into
 * dist/ and referenced from the app's About dialog.
 *
 * Requires: cargo-about, pnpm (all present where Tauri builds run).
 */

import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, '..');             // project root
const SRC_TAURI = join(ROOT, 'src-tauri');          // Rust workspace
const OUT = join(ROOT, 'public', 'THIRD_PARTY_LICENSES.html'); // final artifact
const LICENSES_JSON = join(ROOT, 'pnpmLicenses.json');         // optional pnpm metadata dump

const htmlEscape = (s) =>
  String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');

/** Read a file as UTF-8 text, stripping a leading BOM if present. */
function readUtf8(path) {
  return readFileSync(path, 'utf8').replace(/^\uFEFF/, '');
}

/** Parse a JSON file as UTF-8 (BOM-tolerant). Throws on invalid JSON. */
function readJson(path) {
  return JSON.parse(readUtf8(path));
}

/**
 * Load the optional root pnpmLicenses.json and index it by package name.
 *
 * The authoritative shape on this project is a top-level ARRAY of entries:
 *
 *   [ { "packages": ["tslib", "..."], "license": "<full license text>" }, ... ]
 *
 * Each entry groups the package names (bare, no version) that share one full
 * license text. We attach that text to every package name it lists, so that
 * when a package ships no LICENSE file we can still render its full text here.
 *
 * For robustness we also accept the older grouped-object shape:
 *
 *   { "<SPDX>": [ { name, homepage, license, ... }, ... ] }
 *
 * Returns a Map<packageName, {licenseText, homepage}> or null when the file is
 * absent or unparseable.
 */
function loadPnpmLicenses() {
  if (!existsSync(LICENSES_JSON)) return null;
  let raw;
  try {
    raw = readJson(LICENSES_JSON);
  } catch (e) {
    console.warn(`[licenses] could not parse ${LICENSES_JSON}: ${e.message}`);
    return null;
  }
  const info = new Map();
  const put = (name, rec) => {
    if (!name) return;
    const existing = info.get(name);
    // Prefer a record that actually carries content, and merge a homepage if
    // one shows up where the earlier record had none.
    const merged = {
      licenseText: (existing?.licenseText) || rec.licenseText || null,
      homepage: (existing?.homepage) || rec.homepage || null,
    };
    if (existing && existing.homepage && !merged.homepage) merged.homepage = existing.homepage;
    info.set(name, merged);
  };

  if (Array.isArray(raw)) {
    // Authoritative array shape: each entry is a full license text shared by
    // every package name in its `packages` list.
    for (const entry of raw) {
      if (!entry || typeof entry !== 'object') continue;
      if (Array.isArray(entry.packages)) {
        const text =
          typeof entry.license === 'string' && entry.license.trim().length
            ? entry.license.trim()
            : null;
        for (const name of entry.packages) {
          put(String(name), { licenseText: text, homepage: null });
        }
      } else {
        // A plain per-package entry inside the array.
        put(entry.name, {
          licenseText:
            typeof entry.license === 'string' && entry.license.trim().length
              ? entry.license.trim()
              : null,
          homepage: entry.homepage ?? null,
        });
      }
    }
  } else if (raw && typeof raw === 'object') {
    // Older grouped-by-license object shape.
    for (const entries of Object.values(raw)) {
      if (!entries) continue;
      const list = Array.isArray(entries) ? entries : [entries];
      for (const entry of list) {
        if (!entry || typeof entry !== 'object') continue;
        put(entry.name, {
          licenseText:
            typeof entry.license === 'string' && entry.license.trim().length
              ? entry.license.trim()
              : null,
          homepage: entry.homepage ?? null,
        });
      }
    }
  }

  console.log(`[licenses] loaded ${info.size} package entries from ${LICENSES_JSON}.`);
  return info;
}

function run(cmd, args, opts = {}) {
  const res = spawnSync(cmd, args, {
    encoding: 'utf8',
    cwd: opts.cwd ?? ROOT,
    env: { ...process.env, ...(opts.env || {}) },
  });
  const ok = res.status === 0;
  if (!ok) {
    console.error(`[licenses] command failed: ${cmd} ${args.join(' ')}`);
    console.error(res.stdout ? res.stdout : '');
    console.error(res.stderr ? res.stderr : '');
    process.exitCode = 1;
  }
  return { ok, stdout: res.stdout ?? '', stderr: res.stderr ?? '' };
}

function step1CargoAbout() {
  console.log('[licenses] generating cargo-about page…');
  const { ok } = run(
    'cargo',
    ['about', 'generate', 'about.hbs', '-o', '../public/THIRD_PARTY_LICENSES.html'],
    { cwd: SRC_TAURI }
  );
  if (!ok) {
    console.error('[licenses] cargo about generate failed.');
    process.exit(1);
  }
  if (!existsSync(OUT)) {
    console.error(`[licenses] expected output not found: ${OUT}`);
    process.exit(1);
  }
}

/**
 * pnpm licenses list --json emits a JSON object keyed by license id, each
 * value a list of packages under that license:
 *
 *   {
 *     "MIT":  [ { name, version, repository, path } ],
 *     "Apache-2.0": [ ... ],
 *     ...
 *   }
 *
 * Some pnpm versions emit {"name": "...", ...} entries inside one array.
 * This normalizer handles the common grouped-by-license shape; if it does
 * not match we fall back to a flat listing.
 */
function normalizeGroups(raw) {
  const groups = new Map(); // licenseKey -> [{name, version}]

  const push = (licenseKey, entry) => {
    const key = String(licenseKey || 'Unknown');
    if (!groups.has(key)) groups.set(key, []);
    const name = entry && (entry.name ?? entry.package ?? 'unknown');
    const version = entry && entry.version ? `@${entry.version}` : '';
    let dir = entry && (entry.path ?? entry.dir);
    if (dir && !existsSync(dir)) dir = resolve(dir);
    const rawName = entry && (entry.name ?? entry.package ?? 'unknown');
    const pkgName = String(rawName ?? 'unknown');
    // pnpm licenses list --json carries no `homepage`, but keep any that a
    // custom data source provided so links survive without pnpmLicenses.json.
    const homepage =
      entry && (entry.homepage ?? entry.repository?.url ?? entry.repository)
        ? String(
            (entry.homepage ??
              entry.repository?.url ??
              entry.repository)
          )
        : null;
    groups
      .get(key)
      .push({
        name: String(name) + version,
        pkgName,
        dir: dir || null,
        homepage,
      });
  };

  if (Array.isArray(raw)) {
    for (const entry of raw) push(entry && entry.license, entry);
  } else if (raw && typeof raw === 'object') {
    for (const [licenseKey, entries] of Object.entries(raw)) {
      if (Array.isArray(entries)) {
        for (const entry of entries) push(licenseKey, entry);
      } else if (entries && typeof entries === 'object') {
        push(licenseKey, entries);
      }
    }
  }

  return groups;
}

function parsePnpmLicenses() {
  const { ok, stdout } = run('pnpm', ['licenses', 'list', '--json']);
  if (ok && stdout.trim()) {
    try {
      const groups = normalizeGroups(JSON.parse(stdout));
      if (groups.size > 0) {
        console.log(`[licenses] collected ${groups.size} pnpm license groups via \`pnpm licenses\`.`);
        return groups;
      }
    } catch {
      console.warn('[licenses] could not parse `pnpm licenses list --json`, falling back to scan.');
    }
  }

  // Fallback: scan installed package.json files directly from the store.
  // This avoids depending on pnpm's (sometimes stale) store index and works
  // offline, which is useful when `pnpm licenses` cannot resolve metadata.
  console.warn('[licenses] `pnpm licenses list --json` unavailable; scanning node_modules…');
  return scanNodeModules();
}

/**
 * Walk node_modules/.pnpm/<name>@<version>/node_modules/<name>/package.json
 * and group packages by their declared license. Works offline without pnpm's
 * store index.
 */
function scanNodeModules() {
  const store = join(ROOT, 'node_modules', '.pnpm');
  if (!existsSync(store)) {
    console.warn('[licenses] no node_modules/.pnpm store found; no npm licenses recorded.');
    return null;
  }
  const groups = new Map();

  const collectPkg = (pkgDir) => {
    const pkgFile = join(pkgDir, 'package.json');
    if (!existsSync(pkgFile)) return;
    let pkg;
    try {
      pkg = readJson(pkgFile);
    } catch {
      return;
    }
    if (!pkg || !pkg.name || !pkg.version) return;
    const licenseKey =
      pkg.license ??
      (Array.isArray(pkg.licenses) ? pkg.licenses[0]?.type : null) ??
      'Unknown';
    const key = String(licenseKey || 'Unknown');
    if (!groups.has(key)) groups.set(key, []);
    groups
      .get(key)
      .push({
        name: `${pkg.name}@${pkg.version}`,
        pkgName: pkg.name,
        dir: pkgDir,
        homepage: pkg.homepage ?? null,
      });
  };

  for (const dir of readdirSync(store, { withFileTypes: true })) {
    if (!dir.name.includes('@') || !dir.isDirectory()) continue;
    const pkgRoot = join(store, dir.name, 'node_modules');
    if (!existsSync(pkgRoot)) continue;

    if (dir.name.startsWith('@')) {
      // Scoped package: node_modules/@scope/<name>
      for (const sub of readdirSync(pkgRoot)) {
        collectPkg(join(pkgRoot, sub));
      }
    } else {
      // Plain package: the first segment before "@version"
      collectPkg(join(pkgRoot, dir.name.split('@')[0]));
    }
  }

  console.log(`[licenses] scanned ${groups.size} pnpm license groups from node_modules.`);
  return groups;
}

/**
 * Resolve the actual license text file inside an installed package directory.
 * Prefers an explicit file pointer, then common file names. Returns the full
 * absolute path or null if none can be found.
 */
function findLicenseFile(pkgDir) {
  if (!pkgDir || !existsSync(pkgDir)) return null;
  let pkg = null;
  try {
    pkg = readJson(join(pkgDir, 'package.json'));
  } catch {
    pkg = null;
  }

  // 1. Explicit file pointer(s) from package.json.
  const explicit = [];
  if (pkg) {
    if (typeof pkg.license === 'string' && /\.(md|txt)$/i.test(pkg.license)) {
      explicit.push(pkg.license);
    }
    if (Array.isArray(pkg.licenses)) {
      for (const l of pkg.licenses) {
        if (l && typeof l.file === 'string') explicit.push(l.file);
      }
    }
  }
  for (const rel of explicit) {
    const candidate = join(pkgDir, rel.replace(/^\.\//, ''));
    if (existsSync(candidate)) return candidate;
  }

  // 2. Common license file names (BSD/MIT/Apache duals included).
  const names = [
    'LICENSE',
    'LICENSE.txt',
    'LICENSE.md',
    'LICENSE-MIT',
    'LICENSE-MIT.txt',
    'LICENSE-MIT.md',
    'LICENSE-Apache',
    'LICENSE-Apache-2.0',
    'LICENCE',
    'LICENCE.txt',
    'LICENCE.md',
    'COPYING',
    'COPYING.txt',
  ];
  for (const name of names) {
    const candidate = join(pkgDir, name);
    if (existsSync(candidate)) return candidate;
  }

  // 3. Case-insensitive fallback.
  let entry = null;
  try {
    entry = readdirSync(pkgDir, { withFileTypes: true });
  } catch {
    entry = null;
  }
  if (entry) {
    const match = entry.find(
      (e) => e.isFile() && /^licen[cs]e/i.test(e.name) && /\.(md|txt)?$/i.test(e.name),
    );
    if (match) return join(pkgDir, match.name);
  }
  return null;
}

/**
 * Read and escape a license file for safe display. Returns '' when absent.
 * When no LICENSE file ships in the package, consult pnpmLicenses.json: if the
 * package is listed there we render its full license text from the `license`
 * field, otherwise we warn loudly so a missing text is never silently
 * swallowed.
 */
function readLicenseText(pkgDir, pkgName, metadata) {
  const file = findLicenseFile(pkgDir);
  if (file) {
    try {
      return htmlEscape(readUtf8(file));
    } catch {
      /* fall through to the metadata fallback below */
    }
  }

  // No LICENSE file found — fall back to the full text recorded in
  // pnpmLicenses.json for this package. This is the whole point of that file.
  const meta = metadata?.get(pkgName);
  if (meta && meta.licenseText) {
    return htmlEscape(meta.licenseText);
  }

  // Nothing reducible to text anywhere — log who we expected so the absence is
  // auditable, and don't claim a link-only fallback that isn't useful.
  const ctx = meta
    ? `${pkgName} (${meta.homepage ?? 'no homepage'}, no license text in pnpmLicenses.json)`
    : `${pkgName} (not listed in pnpmLicenses.json)`;
  console.warn(`[licenses] no license TEXT found for ${ctx}; rendering link only.`);
  return '';
}

function buildPnpmSection(groups, metadata) {
  if (!groups || groups.size === 0) {
    console.warn('[licenses] no pnpm license data found; skipping pnpm section.');
    return '';
  }

  const sorted = [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  const items = sorted
    .map(([licenseKey, pkgs]) => {
      // De-duplicate on the display name (name@version), keeping the bare
      // package name for metadata lookups.
      const seen = new Map();
      for (const p of pkgs) seen.set(p.name, p);
      const unique = [...seen.values()].sort((a, b) => a.name.localeCompare(b.name));

      const names = unique
        .map((p) => {
          const label = htmlEscape(p.name);
          // Prefer the homepage recorded with the package itself (from
          // package.json / pnpmLicenses.json data). Fall back to the static
          // metadata map so links render even for command-sourced groups.
          const href = p.homepage ?? metadata?.get(p.pkgName)?.homepage;
          return href
            ? `              <li><a href="${htmlEscape(href)}" target="_blank" rel="noopener">${label}</a></li>`
            : `              <li>${label}</li>`;
        })
        .join('\n');

      // Reuse the first package that actually ships a license text. If none
      // ships a file, readLicenseText warns (and consults pnpmLicenses.json).
      let text = '';
      for (const pkg of unique) {
        const t = readLicenseText(pkg.dir, pkg.pkgName, metadata);
        if (t) {
          text = t;
          break;
        }
      }
      const textBlock = text
        ? `                <pre class="license-text">${text}</pre>\n`
        : '';

      return `        <li class="license">
            <h3 id="pnpm-${htmlEscape(licenseKey)}">${htmlEscape(licenseKey)}</h3>
            <h4>Used by (pnpm):</h4>
            <ul class="license-used-by">
${names}
            </ul>
${textBlock}        </li>`;
    })
    .join('\n');

  return `
        <h2>JavaScript / npm packages (pnpm):</h2>
        <ul class="licenses-list">
${items}
        </ul>`;
}

function inject(doc, section) {
  if (!section) return doc;
  const marker = '</main>';
  const idx = doc.lastIndexOf(marker);
  if (idx === -1) {
    console.error('[licenses] could not locate </main> in generated HTML.');
    return doc;
  }
  return doc.slice(0, idx) + section + '\n    ' + doc.slice(idx);
}

// ── main ────────────────────────────────────────────────────────────────
step1CargoAbout();

const groups = parsePnpmLicenses();
const metadata = loadPnpmLicenses();
const section = buildPnpmSection(groups, metadata);

let doc = readFileSync(OUT, 'utf8');
doc = inject(doc, section);
writeFileSync(OUT, doc, 'utf8');

console.log(`[licenses] wrote ${OUT} (${doc.length} bytes).`);
