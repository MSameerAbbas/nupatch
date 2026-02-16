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

### What the patcher does

**CLI agent** (3 patches):
1. **Nu detection**: Adds `includes("nu")?ShellType.Naive:` **before** the PowerShell condition in `detectShellType()` so nushell is recognized from hints (placement is critical -- after the PowerShell check is unreachable on Windows)
2. **System nu detection**: Adds `commandExists("nu")?ShellType.Naive:` before the final fallback in `detectShellType()` so nushell is detected from PATH even without `$env:SHELL`
3. **Naive case**: Adds `case ShellType.Naive:` in the executor factory with `findActualExecutable("nu")` PATH-based shell resolution (no `$env:SHELL` required)

**IDE agent** (4 patches + integrity chain):
1. **Nu detection**: Same `includes("nu")` detection before the PowerShell condition
2. **System nu detection**: Same `commandExists("nu")` PATH-based check
3. **userTerminalHint**: Wires `userTerminalHint` into the shell resolution function (`ce()`) so the IDE's configured shell path (from `terminal.integrated.defaultProfile.windows`) is used by `NaiveTerminalExecutor`
4. **Shell path fallback**: Adds `case ShellType.Naive:` to `getShellExecutablePath()` (`Se()`) with `findActualExecutable("nu")` and fixes the `default:` case to return PowerShell on Windows instead of `/bin/sh`
5. Updates the SHA-256 hex hash in `extensionHostProcess.js` and the base64 checksum in `product.json`

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
- `<enum>.PowerShell:<enum>.Naive}` — discovers the system detection insertion point
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

- **CLI**: `nu` must be on PATH. No `$env:SHELL` needed — the patcher uses `findActualExecutable("nu")` for auto-discovery.
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
