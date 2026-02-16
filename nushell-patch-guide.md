# Cursor Nushell Patch Guide (Personal Reference)

Local patches to make Cursor's CLI and IDE agents use Nushell instead of PowerShell on Windows.

First verified on: CLI v2026.02.13-41ac335, IDE v2.4.37

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

1. **No nushell detection in `detectShellType()`**: No `includes("nu")` check. Worse, on Windows the PowerShell ternary arm combines both string matching **and** a system-level `commandExists` fallback in one `||` chain: `includes("pwsh") || includes("powershell") || isWindows && (commandExists("pwsh") || commandExists("powershell"))`. Since PowerShell is always installed on Windows, this fires `true` regardless of the hint, making any shell check placed **after** it unreachable dead code.
2. **No `userTerminalHint` in shell resolution**: The shell resolution function (`ce()` in the IDE) doesn't check `userTerminalHint`, so even when detection works, the shell path used is still PowerShell (from a hardcoded Windows resolver `ne()`)
3. **`getShellExecutablePath` returns `/bin/sh` on Windows**: The `Se()` function's `default:` case returns `/bin/sh` for `ShellType.Naive`, which doesn't exist on Windows. This breaks the legacy terminal tool path.
4. **No PATH-based nushell detection**: The system-level fallback in `detectShellType()` only checks for `pwsh`/`powershell`/`zsh`/`bash` -- never `nu`.

The `NaiveTerminalExecutor` class (spawns `shell -c "command"`) already exists and works with nushell. It just needs to be routed to with the correct shell path.

## Minified Code Structure

> Last verified: CLI `agent-cli@2026.02.13-41ac335`, IDE `v2.4.37`.
> Minified names change across versions, but the structure is stable.

### `detectShellType` -- the core routing decision

**CLI** (`qe(e)`) -- string checks and system checks are **separate** arms:
```javascript
function qe(e) {
  if (e === V.ZshLight) return V.ZshLight;
  const t = e || process.env.SHELL || "";
  const n = e ? void 0 : Ue();                    // MSYSTEM git-bash check
  const r = void 0 !== n || /git.*bash\.exe$/i.test(t) || ...;

  // --- hint-based checks (from userTerminalHint / $env.SHELL) ---
  return t.includes("zsh")  ? V.Zsh
       : t.includes("bash") && r ? V.Bash
       : t.includes("pwsh") || t.includes("powershell") ? V.PowerShell
  // --- system-level checks (commandExists probes) ---
       : n ? V.Bash                                      // MSYSTEM bash
       : Qe("pwsh") || Qe("powershell") ? V.PowerShell   // <-- always true on Windows
       : Qe("zsh")  ? V.Zsh
       : Qe("bash") && r ? V.Bash
       : Qe("pwsh") || Qe("powershell") ? V.PowerShell   // redundant duplicate
       : V.Naive;
}
```
Key: the system-level PowerShell check (`Qe("pwsh")||Qe("powershell")`) fires before any `Qe("nu")` could be inserted after it, making everything below it **dead code on Windows**.

**IDE** (`Te(e)`) -- hint + system PowerShell checks are **combined** in one arm:
```javascript
function Te(e) {
  if (e === O.ZshLight) return O.ZshLight;
  const t = e || process.env.SHELL || "";
  const r = "win32" === process.platform;
  const n = /git.*bash\.exe$/i.test(t) || ...;
  const s = !r || n;

  return t.includes("zsh")  ? O.Zsh
       : t.includes("bash") && s ? O.Bash
       // vvv combined: string match OR system probe -- always true on Windows vvv
       : t.includes("pwsh") || t.includes("powershell")
         || r && (Ie("pwsh") || Ie("powershell")) ? O.PowerShell
       // ^^^ everything after this is dead code on Windows ^^^
       : Ie("zsh")  ? O.Zsh
       : Ie("bash") && s ? O.Bash
       : Ie("pwsh") || Ie("powershell") ? O.PowerShell
       : O.Naive;
}
```
The IDE merges the `includes("pwsh")` string check with `isWindows && (commandExists(...))` in a single `||` chain, so the PowerShell arm fires even when `$env.SHELL` is unset.

### `commandExists` -- PATH probe

```javascript
// CLI: Qe(e)          IDE: Ie(e)
function commandExists(e) {
  try { return findActualExecutable(e, []).cmd !== e; }
  catch(e) { return false; }
}
```
Returns `true` if the binary resolves to a real path (i.e. is on PATH).

### Executor factory -- how `ShellType` maps to an executor

**IDE** (`be(e, t)`) -- `default` creates `NaiveTerminalExecutor` via `ce(e)`:
```javascript
switch (t) {
  case O.Zsh:       return new te(fe(e));       // LazyExecutor(zsh setup)
  case O.Bash:      return new te(X(e));        // LazyExecutor(bash setup)
  case O.PowerShell:return new te(ie());        // LazyExecutor(pwsh setup)
  case O.ZshLight:  return new te(we(e));       // LazyExecutor(zsh-light setup)
  default:          return ce(e);               // NaiveTerminalExecutor directly
}
```
No `case O.Naive:` is needed because the `default` already routes there.

**CLI** -- `default` calls `Te(t)` which creates `NaiveTerminalExecutor` (`ve`):
```javascript
switch (t) {
  case V.Zsh:       return new Ae(async function(){ ... });  // LazyExecutor
  case V.Bash:      /* similar */
  case V.PowerShell:return new Ae(async function(){ ... });
  case V.ZshLight:  /* similar */
  default:          return Te(t);   // Te() creates NaiveTerminalExecutor
}
```
The CLI factory has inline async functions instead of helper calls.

### Shell resolution -- `ce()` (IDE) and `Te()` (CLI)

**IDE** `ce(e)`:
```javascript
function ce(e) {
  const t = "win32" === process.platform;
  const r = e?.shell ?? (t ? ne() : void 0);   // ne() = PowerShell path
  const n = process.cwd();
  return new oe(n, { ...e, shell: r });         // oe = NaiveTerminalExecutor
}
```
Without patching, `e.userTerminalHint` is never consulted, so `r` always resolves to PowerShell on Windows.

**CLI** `Te(e)`:
```javascript
function Te(e) {
  const t = e?.shell ?? Ce();     // Ce() = PowerShell path resolver
  const n = process.cwd();
  return new ve(n, { ...e, shell: t });  // ve = NaiveTerminalExecutor
}
```

### `getShellExecutablePath` -- `Se()` (IDE only)

```javascript
function Se(e) {
  switch (e) {
    case O.Zsh:
    case O.ZshLight:  return findActualExecutable("zsh", []).cmd;
    case O.Bash:      return findActualExecutable("bash", []).cmd;
    case O.PowerShell:return ne();              // PowerShell path
    default:          return process.env.SHELL || "/bin/sh";  // broken on Windows
  }
}
```
The `default` case returns `/bin/sh` on Windows, which doesn't exist. Used by the legacy terminal tool path.

### CLI vs IDE structural differences

1. **`detectShellType` PowerShell arm**: The IDE combines hint-based and system-level PowerShell checks in one `||` chain, making **everything** after it unreachable on Windows. The CLI separates them into distinct arms, so inserting between them is possible and effective.

2. **Executor factory `default` case**: The IDE's `default` calls `ce(e)` which directly creates a `NaiveTerminalExecutor` -- no explicit `case Naive:` is needed. The CLI's `default` calls `Te(t)` which similarly creates one, but the CLI patch adds an explicit `case V.Naive:` to inject `findActualExecutable("nu")` for shell resolution (since the CLI doesn't have a separate `Se()` function).

3. **`getShellExecutablePath` (`Se()`)**: Only exists in the IDE. The CLI resolves the shell path inline in `Te()` and in the patched `case V.Naive:` block.

---

### What the patcher does

**CLI agent** (3 patches):
1. **Nu detection**: Adds `includes("nu")?ShellType.Naive:` **before** the PowerShell condition in `detectShellType()` so nushell is recognized from hints (placement is critical -- after the PowerShell check is unreachable on Windows)
2. **System nu detection**: Adds `commandExists("nu")?ShellType.Naive:` right after the first (hint-based) `?PowerShell:` arm in `detectShellType()`, placing it before the system-level PowerShell checks so it's reachable on Windows where PowerShell is always installed
3. **Naive case**: Adds `case ShellType.Naive:` in the executor factory with `findActualExecutable("nu")` PATH-based shell resolution (no `$env:SHELL` required)

**IDE agent** (4 patches + integrity chain):
1. **Nu detection**: Same `includes("nu")` detection before the PowerShell condition
2. **System nu detection**: Same `commandExists("nu")` PATH-based check (on the IDE, placed after the combined hint+system PowerShell arm -- unreachable on Windows, but the IDE relies on `userTerminalHint` instead)
3. **userTerminalHint**: Wires `userTerminalHint` into the shell resolution function (`ce()`) so the IDE's configured shell path (from `terminal.integrated.defaultProfile.windows`) is used by `NaiveTerminalExecutor`
4. **Shell path fallback**: Adds `case ShellType.Naive:` to `getShellExecutablePath()` (`Se()`) with `findActualExecutable("nu")` and fixes the `default:` case to return PowerShell on Windows instead of `/bin/sh`
5. Updates the SHA-256 hex hash in `extensionHostProcess.js` and the base64 checksum in `product.json`

### Before and after: `detectShellType` chain

**CLI -- before** (unpatched):
```javascript
// hint-based
  t.includes("zsh")  ? V.Zsh
: t.includes("bash") && r ? V.Bash
: t.includes("pwsh") || t.includes("powershell") ? V.PowerShell
// system-level
: n ? V.Bash
: Qe("pwsh") || Qe("powershell") ? V.PowerShell   // always true on Windows
: Qe("zsh")  ? V.Zsh                                // dead code on Windows
: Qe("bash") && r ? V.Bash                          // dead code on Windows
: Qe("pwsh") || Qe("powershell") ? V.PowerShell     // dead code on Windows
: V.Naive                                            // dead code on Windows
```

**CLI -- after** (patched, inserted segments marked with `+++`):
```javascript
// hint-based
  t.includes("zsh")  ? V.Zsh
: t.includes("bash") && r ? V.Bash
: t.includes("nu") ? V.Naive                        // +++ nu hint detection
: t.includes("pwsh") || t.includes("powershell") ? V.PowerShell
// system-level
: Qe("nu") ? V.Naive                                // +++ nu system detection (before PS)
: n ? V.Bash
: Qe("pwsh") || Qe("powershell") ? V.PowerShell
: Qe("zsh")  ? V.Zsh
: Qe("bash") && r ? V.Bash
: Qe("pwsh") || Qe("powershell") ? V.PowerShell
: V.Naive
```
The system `Qe("nu")` is inserted **before** `n ? V.Bash` and the first system PowerShell check, so it's reachable on Windows.

**IDE -- before** (unpatched):
```javascript
// hint-based
  t.includes("zsh")  ? O.Zsh
: t.includes("bash") && s ? O.Bash
// combined hint + system (always true on Windows)
: t.includes("pwsh") || t.includes("powershell")
  || r && (Ie("pwsh") || Ie("powershell")) ? O.PowerShell
// everything below is dead code on Windows
: Ie("zsh")  ? O.Zsh
: Ie("bash") && s ? O.Bash
: Ie("pwsh") || Ie("powershell") ? O.PowerShell
: O.Naive
```

**IDE -- after** (patched, inserted segments marked with `+++`):
```javascript
// hint-based
  t.includes("zsh")  ? O.Zsh
: t.includes("bash") && s ? O.Bash
: t.includes("nu") ? O.Naive                        // +++ nu hint detection
// combined hint + system (always true on Windows)
: t.includes("pwsh") || t.includes("powershell")
  || r && (Ie("pwsh") || Ie("powershell")) ? O.PowerShell
// system-level (unreachable on Windows -- IDE relies on userTerminalHint instead)
: Ie("nu") ? O.Naive                                // +++ nu system detection
: Ie("zsh")  ? O.Zsh
: Ie("bash") && s ? O.Bash
: Ie("pwsh") || Ie("powershell") ? O.PowerShell
: O.Naive
```
On the IDE, the system `Ie("nu")` is placed after the combined PowerShell arm. This is unreachable on Windows, but the IDE doesn't need it -- it gets Nushell via `userTerminalHint` (from `terminal.integrated.defaultProfile.windows`) wired into `ce()`.

### Shell resolution chain (after patching)

**CLI Naive case**:
```
userTerminalHint → findActualExecutable("nu") → process.env.SHELL → "/bin/sh"
```

**IDE non-legacy path** (`ce()`):
```
opts.shell → opts.userTerminalHint → platform default (PowerShell on Windows)
```

**IDE legacy path** (`Se(O.Naive)`):
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
- `default:return process.env.SHELL||"/bin/sh"` — discovers the `Se()` fallback to patch

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

Last verified against CLI `agent-cli@2026.02.13-41ac335` and IDE `v2.4.37`. The patcher discovers these dynamically via regex, so they don't need to be updated manually -- this table is for debugging and manual inspection.

| Role | CLI name | IDE name |
|------|----------|----------|
| `hintVar` (detectShellType arg) | `t` | `t` |
| `enumVar` (ShellType enum) | `V` | `O` |
| `LazyExecutor` class | `Ae` | `te` |
| `NaiveTerminalExecutor` class | `ve` | `oe` |
| `commandExists` function | `Qe` | `Ie` |
| `findActualExecutable` call | `(0,r.Ef)` | `(0,s.findActualExecutable)` |
| `detectShellType` function | `qe` | `Te` |
| Executor factory function | *(inline switch)* | `be` |
| Shell resolution (`ce`/`Te`) | `Te` | `ce` |
| `getShellExecutablePath` | *(none)* | `Se` |
