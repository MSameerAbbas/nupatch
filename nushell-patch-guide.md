# Cursor Nushell Patch Guide (Personal Reference)

Local patches to make Cursor's CLI and IDE agents use Nushell instead of PowerShell on Windows.

First verified on: CLI v2026.02.13-41ac335, IDE v2.4.37
Last verified on: CLI v2026.02.27-e7d2ef6, IDE v2.5.26

---

## Quick Start

```
nupatch patch                  # patch both CLI + IDE
nupatch patch --dry-run        # show what would change
nupatch status                 # check current patch state
nupatch revert                 # restore all from backups
```

Then:
- **CLI**: Nushell auto-detected from PATH -- no `$env:SHELL` needed
- **IDE**: Full quit + relaunch Cursor (not just Reload Window)

---

## Overview

Cursor's agents use a shared shell execution library with several bugs:

1. **No nushell detection in `detectShellType()`**: No `includes("nu")` check. On Windows, the system-level PowerShell check (`commandExists("pwsh") || commandExists("powershell")`) always fires `true` since PowerShell is always installed, making everything after it unreachable dead code. Any nushell check placed after this point will never execute.
2. **No `userTerminalHint` in shell resolution**: The shell resolution function (`Ue()` in the IDE, `Se()` in the CLI) doesn't check `userTerminalHint`, so even when detection works, the shell path used is still PowerShell (from a hardcoded Windows resolver `Pe()`/`Ce()`)
3. **`getShellExecutablePath` returns `/bin/sh` on Windows**: The `ot()` function's `default:` case returns `/bin/sh` for `ShellType.Naive`, which doesn't exist on Windows. This breaks the legacy terminal tool path.
4. **No PATH-based nushell detection**: The system-level fallback in `detectShellType()` only checks for `pwsh`/`powershell`/`zsh`/`bash` -- never `nu`.

The `NaiveTerminalExecutor` class (spawns `shell -c "command"`) already exists and works with nushell. It just needs to be routed to with the correct shell path.

## Minified Code Structure

> Last verified: CLI `v2026.02.27-e7d2ef6`, IDE `v2.5.26`.
> Minified names change across versions, but the structure is stable.

### `detectShellType` -- the core routing decision

Both CLI and IDE now have the same structure: hint-based string checks and system-level `commandExists` checks are **separate** arms. (Prior to IDE v2.5.26, the IDE combined hint + system PowerShell checks in one `||` chain, making everything after it unreachable on Windows. That was fixed upstream.)

**CLI** (`Ge(e)`):
```javascript
function Ge(e) {
  if (e === W.ZshLight) return W.ZshLight;
  const t = e || process.env.SHELL || "";
  const n = e ? void 0 : qe();                    // MSYSTEM git-bash check
  const r = void 0 !== n || /git.*bash\.exe$/i.test(t) || ...;

  // --- hint-based checks (from userTerminalHint / $env.SHELL) ---
  return t.includes("zsh")  ? W.Zsh
       : t.includes("bash") && r ? W.Bash
       : t.includes("pwsh") || t.includes("powershell") ? W.PowerShell
  // --- system-level checks (commandExists probes) ---
       : n ? W.Bash                                      // MSYSTEM bash
       : Ue("pwsh") || Ue("powershell") ? W.PowerShell   // <-- always true on Windows
       : Ue("zsh")  ? W.Zsh
       : Ue("bash") && r ? W.Bash
       : Ue("pwsh") || Ue("powershell") ? W.PowerShell   // redundant duplicate
       : W.Naive;
}
```

**IDE** (`ut(e)`):
```javascript
function ut(e) {
  if (e === ne.ZshLight) return ne.ZshLight;
  const t = e || process.env.SHELL || "";
  const n = "win32" === process.platform;
  const r = n && !e ? lt() : void 0;              // MSYSTEM check (Windows + no hint only)
  const s = void 0 !== r || /git.*bash\.exe$/i.test(t) || ...;
  const i = !n || s;

  // --- hint-based checks ---
  return t.includes("zsh")  ? ne.Zsh
       : t.includes("bash") && i ? ne.Bash
       : t.includes("pwsh") || t.includes("powershell") ? ne.PowerShell
  // --- system-level checks ---
       : r ? ne.Bash                                           // MSYSTEM bash
       : n && (ct("pwsh") || ct("powershell")) ? ne.PowerShell // <-- always true on Windows
       : ct("zsh")  ? ne.Zsh
       : ct("bash") && i ? ne.Bash
       : ct("pwsh") || ct("powershell") ? ne.PowerShell
       : ne.Naive;
}
```

Key: in both agents, the system-level PowerShell check always fires on Windows, making everything below it **dead code on Windows**.

### `commandExists` -- PATH probe

```javascript
// CLI: Ue(e)          IDE: ct(e)
function commandExists(e) {
  try { return findActualExecutable(e, []).cmd !== e; }
  catch(e) { return false; }
}
```
Returns `true` if the binary resolves to a real path (i.e. is on PATH).

### Executor factory -- how `ShellType` maps to an executor

Both agents now wire `userTerminalHint` into `detectShellType` before the switch. If `userTerminalHint` is not provided, they fall back to the MSYSTEM check (`lt()`/`qe()`). The hint is then passed to `detectShellType` to determine which executor to create.

**IDE** (`dt(e)`):
```javascript
function dt(e) {
  let t = e;
  if (!e?.userTerminalHint) {
    const n = lt();                                // MSYSTEM fallback
    n && (t = { ...e, userTerminalHint: n });
  }
  switch (ut(t?.userTerminalHint ?? "")) {         // detectShellType(hint)
    case ne.Zsh:       return new Qe(We(t));       // LazyExecutor(zsh setup)
    case ne.Bash:      return new Qe(xe(t));       // LazyExecutor(bash setup)
    case ne.PowerShell:return new Qe(Me());        // LazyExecutor(pwsh setup)
    case ne.ZshLight:  return new Qe(et(t));       // LazyExecutor(zsh-light setup)
    default:           return Ue(t);               // NaiveTerminalExecutor directly
  }
}
```

**CLI** (`He(e)`):
```javascript
function He(e) {
  let t = e;
  if (!e?.userTerminalHint) {
    const n = qe();                                // MSYSTEM fallback
    n && (t = { ...e, userTerminalHint: n });
  }
  switch (Ge(t?.userTerminalHint ?? "")) {         // detectShellType(hint)
    case W.Zsh:       return new Ae(async function(){ ... });  // LazyExecutor
    case W.Bash:      /* similar */
    case W.PowerShell:return new Ae(async function(){ ... });
    case W.ZshLight:  /* similar */
    default:          return Se(t);                // Se() creates NaiveTerminalExecutor
  }
}
```

No `case Naive:` is needed in either factory because `default` already routes there. The CLI patch adds an explicit `case W.Naive:` to inject `findActualExecutable("nu")` for shell resolution (since the CLI doesn't have a separate `getShellExecutablePath`).

### Shell resolution -- `Ue()` (IDE) and `Se()` (CLI)

**IDE** `Ue(e)`:
```javascript
function Ue(e) {
  const t = "win32" === process.platform;
  const n = e?.shell ?? (t ? Pe() : void 0);   // Pe() = PowerShell path
  const r = process.cwd();
  return new Oe(r, { ...e, shell: n });         // Oe = NaiveTerminalExecutor
}
```
Without patching, `e.userTerminalHint` is never consulted, so `n` always resolves to PowerShell on Windows.

**CLI** `Se(e)`:
```javascript
function Se(e) {
  const t = e?.shell ?? Ce();     // Ce() = PowerShell path resolver
  const n = process.cwd();
  return new ve(n, { ...e, shell: t });  // ve = NaiveTerminalExecutor
}
```

### `getShellExecutablePath` -- `ot()` (IDE only)

```javascript
function ot(e) {
  switch (e) {
    case ne.Zsh:
    case ne.ZshLight:  return findActualExecutable("zsh", []).cmd;
    case ne.Bash:      return findActualExecutable("bash", []).cmd;
    case ne.PowerShell:return Pe();              // PowerShell path
    default:           return process.env.SHELL || "/bin/sh";  // broken on Windows
  }
}
```
The `default` case returns `/bin/sh` on Windows, which doesn't exist. Used by the legacy terminal tool path.

### CLI vs IDE structural differences

1. **`detectShellType` structure**: Both agents now have the same structure -- hint-based and system-level checks are separate arms. (Prior to IDE v2.5.26, the IDE combined them in one `||` chain, making everything after the PowerShell arm unreachable on Windows.)

2. **Executor factory `userTerminalHint`**: Both agents now wire `userTerminalHint` into `detectShellType` before the switch. This means `includes("nu")` can fire when the user's configured terminal profile contains "nu". However, the shell resolution functions (`Ue()`/`Se()`) still don't use `userTerminalHint`, so the NaiveTerminalExecutor would still get PowerShell without patching.

3. **`getShellExecutablePath` (`ot()`)**: Only exists in the IDE. The CLI resolves the shell path inline in `Se()` and in the patched `case W.Naive:` block.

---

### What the patcher does

**CLI agent** (3 patches):
1. **Nu detection**: Adds `includes("nu")?ShellType.Naive:` **before** the PowerShell condition in `detectShellType()` so nushell is recognized from hints (placement is critical -- after the PowerShell check is unreachable on Windows)
2. **System nu detection**: Adds `commandExists("nu")?ShellType.Naive:` right after the first (hint-based) `?PowerShell:` arm in `detectShellType()`, placing it before the system-level PowerShell checks so it's reachable on Windows where PowerShell is always installed
3. **Naive case**: Adds `case ShellType.Naive:` in the executor factory with `findActualExecutable("nu")` PATH-based shell resolution (no `$env:SHELL` required)

**IDE agent** (4 patches + integrity chain):
1. **Nu detection**: Same `includes("nu")` detection before the PowerShell condition
2. **System nu detection**: Same `commandExists("nu")` PATH-based check, placed after the hint-based PowerShell arm and before the system-level PowerShell checks -- reachable on Windows
3. **userTerminalHint**: Wires `userTerminalHint` into the shell resolution function (`Ue()`) so the IDE's configured shell path (from `terminal.integrated.defaultProfile.windows`) is used by `NaiveTerminalExecutor`
4. **Shell path fallback**: Adds `case ShellType.Naive:` to `getShellExecutablePath()` (`ot()`) with `findActualExecutable("nu")` and fixes the `default:` case to return PowerShell on Windows instead of `/bin/sh`
5. Updates the SHA-256 hex hash in `extensionHostProcess.js` and the base64 checksum in `product.json`

### Before and after: `detectShellType` chain

**CLI -- before** (unpatched):
```javascript
// hint-based
  t.includes("zsh")  ? W.Zsh
: t.includes("bash") && r ? W.Bash
: t.includes("pwsh") || t.includes("powershell") ? W.PowerShell
// system-level
: n ? W.Bash
: Ue("pwsh") || Ue("powershell") ? W.PowerShell   // always true on Windows
: Ue("zsh")  ? W.Zsh                                // dead code on Windows
: Ue("bash") && r ? W.Bash                          // dead code on Windows
: Ue("pwsh") || Ue("powershell") ? W.PowerShell     // dead code on Windows
: W.Naive                                            // dead code on Windows
```

**CLI -- after** (patched, inserted segments marked with `+++`):
```javascript
// hint-based
  t.includes("zsh")  ? W.Zsh
: t.includes("bash") && r ? W.Bash
: t.includes("nu") ? W.Naive                        // +++ nu hint detection
: t.includes("pwsh") || t.includes("powershell") ? W.PowerShell
// system-level
: Ue("nu") ? W.Naive                                // +++ nu system detection (before PS)
: n ? W.Bash
: Ue("pwsh") || Ue("powershell") ? W.PowerShell
: Ue("zsh")  ? W.Zsh
: Ue("bash") && r ? W.Bash
: Ue("pwsh") || Ue("powershell") ? W.PowerShell
: W.Naive
```
The system `Ue("nu")` is inserted **before** `n ? W.Bash` and the first system PowerShell check, so it's reachable on Windows.

**IDE -- before** (unpatched):
```javascript
// hint-based
  t.includes("zsh")  ? ne.Zsh
: t.includes("bash") && i ? ne.Bash
: t.includes("pwsh") || t.includes("powershell") ? ne.PowerShell
// system-level
: r ? ne.Bash                                           // MSYSTEM bash
: n && (ct("pwsh") || ct("powershell")) ? ne.PowerShell // always true on Windows
: ct("zsh")  ? ne.Zsh                                   // dead code on Windows
: ct("bash") && i ? ne.Bash                              // dead code on Windows
: ct("pwsh") || ct("powershell") ? ne.PowerShell         // dead code on Windows
: ne.Naive                                               // dead code on Windows
```

**IDE -- after** (patched, inserted segments marked with `+++`):
```javascript
// hint-based
  t.includes("zsh")  ? ne.Zsh
: t.includes("bash") && i ? ne.Bash
: t.includes("nu") ? ne.Naive                        // +++ nu hint detection
: t.includes("pwsh") || t.includes("powershell") ? ne.PowerShell
// system-level
: ct("nu") ? ne.Naive                                // +++ nu system detection (before PS)
: r ? ne.Bash
: n && (ct("pwsh") || ct("powershell")) ? ne.PowerShell
: ct("zsh")  ? ne.Zsh
: ct("bash") && i ? ne.Bash
: ct("pwsh") || ct("powershell") ? ne.PowerShell
: ne.Naive
```
The system `ct("nu")` is inserted **before** `r ? ne.Bash` and the system PowerShell check, so it's reachable on Windows. (In previous IDE versions, the combined PowerShell arm made this position unreachable -- the IDE relied solely on `userTerminalHint` for Windows nushell detection.)

### Shell resolution chain (after patching)

**CLI Naive case**:
```
userTerminalHint → findActualExecutable("nu") → process.env.SHELL → "/bin/sh"
```

**IDE non-legacy path** (`Ue()`):
```
opts.shell → opts.userTerminalHint → platform default (PowerShell on Windows)
```

**IDE legacy path** (`ot(ne.Naive)`):
```
findActualExecutable("nu") → process.env.SHELL → PowerShell on Windows / "/bin/sh" on *nix
```

---

## The Automated Patcher

`nupatch` (Rust) uses **regex-based pattern discovery** to find the minified variable names dynamically. It doesn't hardcode version-specific names like `qe`, `Ge`, `ve` etc. Instead it finds them by matching structural patterns that are stable across versions:

- `<hint>.includes("zsh")?<enum>.Zsh` — discovers `hintVar` and `enumVar`
- `case <enum>.Zsh:...new <LazyExec>(` — discovers `LazyExecutor`
- `new <NaiveExec>(<cwd>, {..., shell:})` — discovers `NaiveTerminalExecutor`
- `function <func>(<arg>){try{return(0,<mod>.findActualExecutable)(<arg>,[]).cmd!==<arg>}` — discovers `cmdExists` function name and `findActualExecutable` call pattern
- `<hint>.includes("pwsh")` — discovers the nu-detection insertion point (must be before this)
- `?<enum>.PowerShell:` (first occurrence in detectShellType) — discovers the system detection insertion point (insert right after it, before the system-level PowerShell checks)
- `<var>?.shell??` — discovers the shell resolution insertion point for `userTerminalHint`
- `default:return process.env.SHELL||"/bin/sh"` — discovers the `getShellExecutablePath` fallback to patch

### Commands

```
nupatch patch                  # patch both CLI + IDE
nupatch patch --cli-only       # patch CLI only
nupatch patch --ide-only       # patch IDE only
nupatch patch --dry-run        # show what would change
nupatch revert                 # restore all from backups
nupatch status                 # check current patch state
```

### After patching

- **CLI**: `nu` must be on PATH. No `$env:SHELL` needed — `detectShellType` checks `commandExists("nu")` before the system-level PowerShell checks, and the Naive case uses `findActualExecutable("nu")` for shell path resolution.
- **IDE**: Full quit + relaunch (check system tray for lingering Cursor processes)
- **IDE settings**: Set `terminal.integrated.defaultProfile.windows` to your Nushell profile. Use literal paths (not `${env:USERPROFILE}`) to avoid VS Code variable resolution issues.

---

## Integrity Check Details (IDE only)

The IDE agent code is protected by a two-layer integrity check. The patcher handles this automatically, but here's how it works:

### Layer 1: Extension hash map

`extensionHostProcess.js` has a variable containing a map like:

```javascript
// Stored without quotes on some keys (minified object literal):
// cursor-agent-exec":{dist:{"main.js":"<sha256-hex-hash>","621.js":"..."}}
{
  "cursor-agent-exec": {
    dist: {
      "main.js": "<sha256-hex-hash>"
    }
  },
  // ... other extensions ...
}
```

Checked by `_verifyExtensionFiles()` at extension load time. On mismatch, the extension **silently fails** (agent times out).

### Layer 2: product.json checksums

`product.json` has a `checksums` section:

```json
{
  "checksums": {
    "vs/workbench/api/node/extensionHostProcess.js": "<sha256-base64-no-padding>",
    // ... 5 other core files ...
  }
}
```

Checked by `FileIntegrityService._isPure()` in `workbench.desktop.main.js` at startup. The checksum service uses:
```javascript
crypto.createHash('sha256').update(content).digest('base64').replace(/=+$/, '')
```
On mismatch, shows "Your Cursor installation appears to be corrupt" warning.

### 3-Step patch chain

1. Patch `cursor-agent-exec/dist/main.js` (all 4 patches)
2. Compute new SHA-256 hex hash, replace old hash in `extensionHostProcess.js`
3. Compute new SHA-256 base64 (stripped `=`) hash of `extensionHostProcess.js`, update in `product.json`

### Files involved

| File | Purpose |
|------|---------|
| `...\extensions\cursor-agent-exec\dist\main.js` | Shell execution library (patched) |
| `...\out\vs\workbench\api\node\extensionHostProcess.js` | SHA-256 **hex** hash map of extensions |
| `...\product.json` | SHA-256 **base64** (no trailing `=`) checksums of core files |

All under `C:\Users\<user>\AppData\Local\Programs\cursor\resources\app\`.

---

## Re-applying After Cursor Updates

Cursor updates overwrite all patched files. `.bak` files from the old version should be deleted first. Then re-run:

```
nupatch patch
nupatch status
```

The regex-based discovery should handle new minified names automatically. If it fails (code was restructured), the patcher prints what it found and what it couldn't find, along with what to search for manually.

**Manual discovery tips** (if regex fails):
- `detectShellType`: search for `includes("zsh")` near `includes("bash")` near `includes("powershell")`
- `commandExists` function: search for `findActualExecutable` near `cmd!==`
- Executor factory: search for `case <enum>.Zsh:` / `case <enum>.Bash:` / `case <enum>.PowerShell:`
- Shell resolution: search for `?.shell??`
- `getShellExecutablePath`: search for `process.env.SHELL||"/bin/sh"`

---

## Discovered Variable Names Reference

Last verified against CLI `v2026.02.27-e7d2ef6` and IDE `v2.5.26`. The patcher discovers these dynamically via regex, so they don't need to be updated manually -- this table is for debugging and manual inspection.

| Role | CLI name | IDE name |
|------|----------|----------|
| `hintVar` (detectShellType arg) | `t` | `t` |
| `enumVar` (ShellType enum) | `W` | `ne` |
| `LazyExecutor` class | `Ae` | `Qe` |
| `NaiveTerminalExecutor` class | `ve` | `Oe` |
| `commandExists` function | `Ue` | `ct` |
| `findActualExecutable` call | `(0,r.findActualExecutable)` | `(0,s.findActualExecutable)` |
| `detectShellType` function | `Ge` | `ut` |
| Executor factory function | `He` | `dt` |
| Shell resolution (`Ue`/`Se`) | `Se` | `Ue` |
| `getShellExecutablePath` | *(none)* | `ot` |
