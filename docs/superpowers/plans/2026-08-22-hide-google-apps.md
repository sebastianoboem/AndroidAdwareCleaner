# Hide Google Apps Filter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a toolbar checkbox “Nascondi Google” (default on) that hides `com.google.android.*` and `com.android.chrome` from the package list without re-scanning.

**Architecture:** Pure helper `isGoogleApp()` in `src/googleApps.mjs` (single source for app + tests; optional thin `src/googleApps.ts` re-export if needed), applied in existing `getFilteredPackages()` alongside hide-system / only-suspicious / text search. Client-side only; no ADB or DB changes.

**Tech Stack:** TypeScript frontend (`src/main.ts`, `index.html`), Node built-in test runner (`node --test`).

**Spec:** `docs/superpowers/specs/2026-08-22-hide-google-apps-design.md`

## Global Constraints

- Match only: package starts with `com.google.android.` OR package equals `com.android.chrome`
- Default: hide (checkbox checked)
- Do not change scan scope / force re-scan
- Combine with other filters via AND
- No persistence across restarts
- No visual grouping UI
- Italian label: `Nascondi Google`

## File Structure

| File | Responsibility |
|---|---|
| `src/googleApps.mjs` | Single source: `isGoogleApp(packageName)` |
| `scripts/test-google-apps.mjs` | Behavior tests importing `../src/googleApps.mjs` |
| `tsconfig.json` | `allowJs: true` so TS can import `.mjs` |
| `index.html` | `#hide-google` checkbox in toolbar |
| `src/main.ts` | Wire helper into filters, events, disable controls |
| `package.json` | `test:unit` script |
| `README.md` | One-line mention of the filter |

---

### Task 1: `isGoogleApp` helper (TDD)

**Files:**
- Create: `src/googleApps.mjs`
- Create: `scripts/test-google-apps.mjs`
- Modify: `package.json`
- Modify: `tsconfig.json` (`"allowJs": true`)

**Interfaces:**
- Produces: `export function isGoogleApp(packageName)` from `src/googleApps.mjs` (single source; app and tests both import this file)

- [ ] **Step 1: Write the failing test**

Create `scripts/test-google-apps.mjs`:

```js
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { isGoogleApp } from "../src/googleApps.mjs";

describe("isGoogleApp", () => {
  it("matches com.google.android.*", () => {
    assert.equal(isGoogleApp("com.google.android.gms"), true);
    assert.equal(isGoogleApp("com.google.android.apps.maps"), true);
    assert.equal(isGoogleApp("com.google.android.youtube"), true);
  });

  it("matches Chrome exactly", () => {
    assert.equal(isGoogleApp("com.android.chrome"), true);
  });

  it("rejects other com.google.* without .android", () => {
    assert.equal(isGoogleApp("com.google.ar.core"), false);
    assert.equal(isGoogleApp("com.google.protobuf"), false);
  });

  it("rejects unrelated packages", () => {
    assert.equal(isGoogleApp("com.samsung.android.app"), false);
    assert.equal(isGoogleApp("com.android.vending"), false);
    assert.equal(isGoogleApp("com.google"), false);
    assert.equal(isGoogleApp("com.google.android"), false);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

```bash
node --test scripts/test-google-apps.mjs
```

Expected: FAIL — cannot find module `../src/googleApps.mjs` (ENOENT).

- [ ] **Step 3: Implement `src/googleApps.mjs` and enable JS in TS**

`src/googleApps.mjs`:

```js
/** True for Google consumer apps: com.google.android.* and Chrome. */
export function isGoogleApp(packageName) {
  return (
    packageName.startsWith("com.google.android.") ||
    packageName === "com.android.chrome"
  );
}
```

In `tsconfig.json` → `compilerOptions`, add `"allowJs": true` (needed so Task 2 can `import { isGoogleApp } from "./googleApps.mjs"`).

- [ ] **Step 4: Run tests to verify they pass**

```bash
node --test scripts/test-google-apps.mjs
```

Expected: all tests pass.

- [ ] **Step 5: Add npm script and commit**

Add to `package.json` → `scripts`:

```json
"test:unit": "node --test scripts/test-google-apps.mjs"
```

```bash
git add src/googleApps.mjs scripts/test-google-apps.mjs package.json tsconfig.json docs/superpowers/plans/2026-08-22-hide-google-apps.md
git commit -m "Add isGoogleApp helper for Google package filter."
```

**Note:** Task 2 must import from `./googleApps.mjs` (not `./googleApps`).

---

### Task 2: Toolbar checkbox + wire into filters

**Files:**
- Modify: `index.html` (toolbar ~L41–48)
- Modify: `src/main.ts` (`getFilteredPackages` ~L689–700, `setScanControlsDisabled` ~L799–804, listeners ~L1233–1241)

**Interfaces:**
- Consumes: `isGoogleApp(packageName: string): boolean` from `./googleApps.mjs`
- Produces: `#hide-google` (default checked); AND-filter in `getFilteredPackages`

- [ ] **Step 1: Add checkbox to `index.html`**

After the “Nascondi sistema” `</label>`, before “Solo sospette”, insert:

```html
            <label class="checkbox-label">
              <input id="hide-google" type="checkbox" checked />
              Nascondi Google
            </label>
```

- [ ] **Step 2: Import and filter in `src/main.ts`**

Add with the other imports at the top of `src/main.ts`:

```ts
import { isGoogleApp } from "./googleApps.mjs";
```

Replace `getFilteredPackages` with:

```ts
function getFilteredPackages(): PackageRow[] {
  const filter = ($("#filter-text") as HTMLInputElement).value.toLowerCase();
  const hideSystem = ($("#hide-system") as HTMLInputElement).checked;
  const hideGoogle = ($("#hide-google") as HTMLInputElement).checked;
  const onlySuspicious = ($("#filter-suspicious") as HTMLInputElement).checked;

  return packages.filter((p) => {
    if (hideSystem && (p.is_system || p.marked_system)) return false;
    if (hideGoogle && isGoogleApp(p.package_name)) return false;
    if (onlySuspicious && !p.is_suspicious) return false;
    const hay = `${p.package_name} ${p.label ?? ""} ${p.author ?? ""}`.toLowerCase();
    return hay.includes(filter);
  });
}
```

- [ ] **Step 3: Disable during scan + change listener**

Replace `setScanControlsDisabled` with:

```ts
function setScanControlsDisabled(disabled: boolean) {
  ($("#hide-system") as HTMLInputElement).disabled = disabled;
  ($("#hide-google") as HTMLInputElement).disabled = disabled;
  ($("#filter-suspicious") as HTMLInputElement).disabled = disabled;
  ($("#filter-text") as HTMLInputElement).disabled = disabled;
  ($("#btn-rescan") as HTMLButtonElement).disabled = disabled;
}
```

After the `#filter-suspicious` listener (~L1238–1241), add:

```ts
  $("#hide-google")?.addEventListener("change", () => {
    updateScanInfo();
    renderTable();
  });
```

- [ ] **Step 4: Typecheck + unit tests**

```bash
npx tsc --noEmit
npm run test:unit
```

Expected: both exit 0.

- [ ] **Step 5: Commit**

```bash
git add index.html src/main.ts
git commit -m "Add Nascondi Google checkbox filter (default on)."
```

---

### Task 3: README + manual acceptance

**Files:**
- Modify: `README.md` (section “### 2. Scansiona e filtra”)

**Interfaces:** none new

- [ ] **Step 1: Update README**

Replace the paragraph under “### 2. Scansiona e filtra” with:

```markdown
Lista app con **icona**, nome, autore e package. Filtri per nascondere sistema e app Google (`com.google.android.*` + Chrome, default nascoste), cercare per nome e mostrare solo le sospette.
```

- [ ] **Step 2: Manual acceptance checklist**

Run `npm run tauri dev` (device connected) or use an existing scanned list.

Verify:
1. With “Nascondi Google” checked, no `com.google.android.*` / `com.android.chrome` rows appear.
2. Uncheck → those rows appear without re-scan.
3. Recheck → hidden again.
4. “Solo sospette” + hide Google: a suspicious Google app appears only when Google hide is off.
5. Scan info count matches visible rows.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "Document Nascondi Google filter in README."
```

---

## Spec coverage (self-review)

| Spec requirement | Task |
|---|---|
| Match `com.google.android.*` + Chrome | Task 1 (single `.mjs` source) |
| Reject other `com.google.*` | Task 1 |
| Checkbox default on | Task 2 |
| AND with other filters | Task 2 |
| No re-scan | Task 2 |
| Disable during operations | Task 2 |
| Visible counts | existing `updateScanInfo` + Task 2 |
| Acceptance criteria 1–5 | Task 3 |
| Out of scope not implemented | — |
