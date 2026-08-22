# Hide Google apps filter

Date: 2026-08-22  
Status: approved design (pending user review of this spec)

## Goal

Let the user hide or show Google apps in the package list, with **hide** as the default. Same UX pattern as “Nascondi sistema”: a toolbar checkbox that only filters the visible table.

## Matching rules

A package is treated as a Google app when **either**:

1. `package_name` starts with `com.google.android.` (case-sensitive, Android package IDs are lowercase), or
2. `package_name` is exactly `com.android.chrome`

Examples:

| Package | Hidden when “Nascondi Google” is on? |
|---|---|
| `com.google.android.gms` | yes |
| `com.google.android.apps.maps` | yes |
| `com.android.chrome` | yes |
| `com.google.android.youtube` | yes |
| `com.google.ar.core` (no `.android` after `com.google`) | no |
| `com.google.protobuf` | no |
| `com.samsung.android.app` | no |

## Behavior

- New checkbox **Nascondi Google** (`#hide-google`), default **checked**.
- When checked, rows matching the rules above are excluded from `getFilteredPackages()`.
- Combines with existing filters with AND logic: hide system, only suspicious, text search.
- Does **not** change ADB scan scope or force a re-scan. Packages stay in memory; unchecking shows them immediately.
- Scan/operation disablement: `#hide-google` is disabled together with `#hide-system` and `#filter-suspicious`.
- `updateScanInfo()` continues to count only visible rows (existing behavior).

## Implementation surface

| File | Change |
|---|---|
| `index.html` | Add `#hide-google` checkbox next to `#hide-system` |
| `src/main.ts` | `isGoogleApp()`, filter in `getFilteredPackages`, change listener, disable in `setScanControlsDisabled` |

No Rust/backend, DB, or sync changes.

## Out of scope

- Visual grouping / collapsible “Google apps” section
- Persisting the checkbox across app restarts
- Broader Google-related package lists beyond the rules above
- Changing how “Nascondi sistema” triggers full vs user-only scans

## Acceptance criteria

1. On first open after scan, Google apps (`com.google.android.*` and Chrome) are not listed while “Nascondi Google” is checked.
2. Unchecking the box shows those apps without re-scanning.
3. Rechecking hides them again.
4. With “Solo sospette” on, a suspicious Google app appears only if “Nascondi Google” is unchecked.
5. Text search still works on the visible set only.
