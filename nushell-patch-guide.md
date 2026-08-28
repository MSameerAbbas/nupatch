# Cursor Nushell Patch Guide

How and why `nupatch` makes Cursor's CLI and IDE agents run [Nushell](https://www.nushell.sh/)
instead of PowerShell on Windows.

This document is self-contained: read it top to bottom and you'll understand the
problem, how Cursor's shell layer works, exactly what each patch changes, and the
two subtle traps that make a naive patch fail. No need to reverse-engineer the
minified code yourself.

Verified against **CLI `v2026.08.25-3e8eec8`** and **IDE `v2.5.26`**. Cursor ships
this code heavily minified, and the identifier names change between releases, so
throughout this guide we use *role names* (e.g. `detectShellType`, `NaiveExecutor`)
rather than the minified letters. `nupatch` recovers the real names automatically
(see [How nupatch finds the code](#6-how-nupatch-finds-the-code)).

---

## 1. The problem

Cursor runs agent shell commands through a small shell-execution library. On
Windows that library has four bugs that together make Nushell impossible to use,
even though Cursor already contains everything needed to run it:

1. **No nushell in shell detection.** The function that maps a shell hint to a
   shell type has no `nu` case, so Nushell is never recognized.
2. **The system fallback never reaches nushell.** Detection ends with system
   probes like `commandExists("pwsh")`. PowerShell is *always* installed on
   Windows, so that probe always returns true — every branch after it is dead
   code. Any `nu` check placed after it can never run.
3. **The shell path always resolves to PowerShell.** When Cursor builds the
   fallback executor it ignores the user's configured terminal
   (`userTerminalHint`) and hard-codes the Windows PowerShell path.
4. **`getShellExecutablePath` returns `/bin/sh` on Windows.** Its default case
   returns a path that doesn't exist on Windows, breaking the legacy terminal
   tool for any unrecognized shell.

The good news: Cursor already has a `NaiveExecutor` that simply spawns
`shell -c "command"`, and it works perfectly with Nushell. The whole job is to
(a) make detection route to it and (b) make the shell path resolve to `nu`.

---

## 2. How Cursor decides which shell to run

The pipeline, from a command to a spawned process:

```
detectShellType(hint) ──▶ ShellType ──▶ executor factory ──▶ an executor ──▶ spawn(shell, [...args, "-c", cmd])
```

### `detectShellType(hint) -> ShellType`

Returns a `ShellType` enum value (`Zsh`, `Bash`, `PowerShell`, `Naive`, …). It
checks the hint string first, then falls back to system PATH probes:

```javascript
// hint-based checks (from userTerminalHint or $env.SHELL)
  hint.includes("zsh")                          ? ShellType.Zsh
: hint.includes("bash") && isBashOk             ? ShellType.Bash
: hint.includes("pwsh")||hint.includes("powershell") ? ShellType.PowerShell
// system-level checks (probe the PATH)
: commandExists("pwsh")||commandExists("powershell") ? ShellType.PowerShell  // ← always true on Windows
: commandExists("zsh")                          ? ShellType.Zsh              // dead code on Windows
: … : ShellType.Naive
```

The marked line is the crux of bug #2: on Windows everything below it is
unreachable.

### `commandExists(name) -> bool`

A PATH probe: `findActualExecutable(name, []).cmd !== name`. It returns true when
`name` resolves to a real binary on PATH. (`findActualExecutable` returns the
input unchanged when it finds nothing, so `cmd !== name` means "found".)

### The executor factory

Wires `userTerminalHint` into detection, then switches on the result:

```javascript
switch (detectShellType(opts?.userTerminalHint ?? "")) {
  case ShellType.Zsh:        return new LazyExecutor(zshSetup(opts));   // env "state dump"
  case ShellType.Bash:       return new LazyExecutor(bashSetup(opts));
  case ShellType.PowerShell: return new LazyExecutor(pwshSetup());
  case ShellType.ZshLight:   return new LazyExecutor(zshLightSetup(opts));
  default:                   return createNaiveExecutor(opts);         // ← Naive lives here
}
```

The zsh/bash/pwsh cases build a `LazyExecutor` that first runs a big script to
capture the shell's environment (this is the slow "warm-up" you see at startup).
The `default` case just makes a `NaiveExecutor` — no warm-up. **Nushell rides the
`default`/Naive path**, which is why it starts instantly.

### `createNaiveExecutor(opts)` — the shell resolver

Builds the `NaiveExecutor` and chooses its shell path:

```javascript
function createNaiveExecutor(opts) {
  const shell = opts?.shell ?? powerShellPath();   // ← ignores userTerminalHint → always pwsh on Windows
  return new NaiveExecutor(process.cwd(), { ...opts, shell });
}
```

This is bug #3: `userTerminalHint` is never consulted, so the shell is PowerShell.

### `getShellExecutablePath(type) -> string`

A separate resolver used by the *legacy* terminal tool. **Both** the CLI and IDE
now ship this function:

```javascript
function getShellExecutablePath(type) {
  switch (type) {
    case ShellType.Zsh:
    case ShellType.ZshLight:  return findActualExecutable("zsh", []).cmd;
    case ShellType.Bash:      return findActualExecutable("bash", []).cmd;
    case ShellType.PowerShell:return powerShellPath();
    default:                  return process.env.SHELL || "/bin/sh";   // ← bug #4: "/bin/sh" on Windows
  }
}
```

### The spawn

The `NaiveExecutor` finally spawns:

```javascript
spawn(this.options?.shell || process.env.SHELL || "/bin/sh",
      [...this.options?.shellArgs ?? [], "-c", command], …)
```

So if `options.shell` is `nu`, this runs `nu -c "command"` — exactly what we want.

---

## 3. The patches

Five patches per agent. Each is a small, targeted string insertion; `nupatch`
applies them to a pristine copy (it restores from backup first, so re-running is
idempotent).

| # | Patch | CLI | IDE | What it does |
|---|-------|:---:|:---:|--------------|
| 1 | Nu detection (hint) | ✅ | ✅ | Recognize `nu` from the hint |
| 2 | Nu detection (system) | ✅ | ✅ | Recognize `nu` from PATH, reachably |
| 3 | Naive factory case | ✅ | — | Resolve shell to `nu` in the executor factory |
| 3′ | userTerminalHint | — | ✅ | Make the shell resolver honor the configured terminal |
| 4 | Shell-path fallback | ✅ | ✅ | Fix `getShellExecutablePath` for Naive + Windows |
| 5 | Nu login flag | ✅ | ✅ | Spawn `nu -l -c` so `env.nu`/`config.nu` load |

### Patch 1 — hint-level nu detection

Insert a `nu` arm **before** the PowerShell hint arm (placement matters: after it
is unreachable on Windows):

```javascript
: hint.includes("bash") && isBashOk ? ShellType.Bash
: hint.includes("nu") ? ShellType.Naive                        // +++ inserted
: hint.includes("pwsh")||hint.includes("powershell") ? ShellType.PowerShell
```

### Patch 2 — system-level nu detection

Insert a `commandExists("nu")` arm right after the *first* (hint-based)
`? ShellType.PowerShell :`, which places it **before** the always-true system
PowerShell probe, keeping it reachable on Windows:

```javascript
: hint.includes("pwsh")||hint.includes("powershell") ? ShellType.PowerShell
: commandExists("nu") ? ShellType.Naive                        // +++ inserted (reachable)
: commandExists("pwsh")||commandExists("powershell") ? ShellType.PowerShell
```

With patches 1–2, `detectShellType("")` returns `Naive` whenever `nu` is on PATH.

### Patch 3 — Naive case in the executor factory (CLI only)

Add an explicit `case ShellType.Naive:` that resolves the shell to `nu`:

```javascript
case ShellType.Naive: {
  const found = findActualExecutable("nu", []).cmd;            // real nu path, or "nu" if not found
  return new NaiveExecutor(process.cwd(), {
    ...opts,
    shell: opts?.userTerminalHint
        || (found !== "nu" ? found : undefined)                // discovered nu path
        || opts?.shell || process.env.SHELL || "/bin/sh",
  });
}
```

Priority: the user's configured terminal, then an auto-discovered `nu`, then
existing fallbacks. See [Trap A and Trap B](#4-two-traps) for why this exact shape
matters.

### Patch 3′ — userTerminalHint in the shell resolver (IDE only)

The IDE reaches the Naive path through `createNaiveExecutor`, so instead of a
factory case we make that resolver honor the configured terminal — add a
`userTerminalHint` fallback to the shell selection:

```javascript
const shell = opts?.shell ?? opts?.userTerminalHint ?? powerShellPath();   // +++ userTerminalHint added
```

Now the IDE uses the shell from `terminal.integrated.defaultProfile.windows`.

### Patch 4 — shell-path fallback (both agents)

Fix `getShellExecutablePath` so the Naive/legacy path resolves `nu` and never
returns `/bin/sh` on Windows:

```javascript
case ShellType.Naive: {                                        // +++ inserted
  const found = findActualExecutable("nu", []).cmd;
  if (found !== "nu") return found;
}
default: return process.env.SHELL
      || ("win32" === process.platform ? powerShellPath() : "/bin/sh");   // +++ Windows-safe
```

### Patch 5 — nu login flag

The `NaiveExecutor` spawns `shell [...args] -c cmd`. When the shell is `nu`,
prepend `-l` so login files (`env.nu`, `config.nu`) load; other shells are
untouched:

```javascript
[...this.options?.shellArgs ?? [],
 .../(?:^|[\\/])nu(?:\.exe)?$/i.test(this.options?.shell || process.env.SHELL || "/bin/sh") ? ["-l"] : [],
 "-c", command]
```

---

## 4. Two traps

These are the mistakes a hand-written patch (or an LLM) will make. Both are
already handled by `nupatch`; they're documented so nobody reintroduces them.

### Trap A — anchor the factory case on the right `switch`

Current builds contain **two** `case ShellType.Zsh:` blocks: one inside
`getShellExecutablePath`, and one inside the executor factory. If Patch 3 anchors
on "the first `case Zsh:`", it lands in `getShellExecutablePath` and the factory
never gets a Naive case — the shell silently resolves wrong.

Fix: anchor on the factory's `switch(detectShellType(opts?.userTerminalHint …))`
statement, then find `case Zsh:` *after* that point.

### Trap B — build the Naive executor exactly like the original

Two details in Patch 3 are load-bearing:

1. **Spread order.** Write `{ ...opts, shell }` — spread first, `shell` last — so
   the resolved shell wins. `{ shell, ...opts }` lets a stray `shell` key in
   `opts` (even `undefined`) clobber it, and the executor falls back to
   `/bin/sh`, which hangs on Windows.
2. **No LazyExecutor wrapper.** Return a bare `new NaiveExecutor(...)`, like the
   factory's `default` case does. The `LazyExecutor` constructor takes a *thunk*
   (`() => Promise<Executor>`) and calls it later; hand it a value (or a
   `Promise`) and it throws/stalls when first used — the command appears to hang.

---

## 5. Integrity: `product.json` checksums

Current Cursor guards its core files with one integrity mechanism that matters
for us: `FileIntegrityService` verifies the SHA-256 checksums in `product.json`
at startup, and on mismatch shows "Your Cursor installation appears to be
corrupt."

Patching the IDE agent changes a file whose checksum lives in `product.json`, so
after patching `nupatch` recomputes every `product.json` checksum from the files
on disk and writes them back. The checksum format is:

```javascript
crypto.createHash('sha256').update(content).digest('base64').replace(/=+$/, '')
```

`nupatch status` reports `product.json checksums: ALL MATCH` when this is correct.
The `verify` and `fix-checksums` commands check and repair it independently.

> Historical note: older Cursor builds also embedded a SHA-256 of the agent's
> `main.js` inside `extensionHostProcess.js` and verified it at load. Current
> builds dropped that per-file hash map, so `nupatch` no longer maintains it.

---

## 6. How nupatch finds the code

`nupatch` never hard-codes minified names. It recovers them from *structural*
patterns that survive renames, then builds each patch from the recovered names:

| Discovered | From the pattern |
|------------|------------------|
| hint var, ShellType enum | `<hint>.includes("zsh")?<enum>.Zsh` |
| LazyExecutor class | `case <enum>.Zsh: … new <class>(` |
| NaiveExecutor class | `new <class>(<cwd>, { …, shell: })` |
| commandExists fn + findActualExecutable call | `function <fn>(a){try{return <call>(a,[]).cmd!==a}` |
| nu-detection insertion point | `<hint>.includes("pwsh")` (insert before) |
| system-detection insertion point | first `?<enum>.PowerShell:` (insert after) |
| factory Naive case anchor | `switch(<fn>(<opts>?.userTerminalHint …)` |
| userTerminalHint insertion point | `<var>?.shell??` |
| shell-path fallback | `default:return process.env.SHELL||"/bin/sh"` |

If a future Cursor release restructures the code (not just renames), discovery
fails loudly and `nupatch` prints what it found and what it couldn't, so you can
locate the change. To search manually:

- `detectShellType`: `includes("zsh")` near `includes("bash")` near `includes("powershell")`
- `commandExists`: `findActualExecutable` near `cmd!==`
- executor factory / getShellExecutablePath: `case <enum>.Zsh:` / `case <enum>.PowerShell:`
- shell resolver: `?.shell??`
- shell-path fallback: `process.env.SHELL||"/bin/sh"`

---

## 7. Files, backups, and re-applying

Patched files (all under `…\resources\app\` and the CLI agent's versioned dir):

| File | Role |
|------|------|
| `cursor-agent/versions/<latest>/index.js` | CLI agent shell library |
| `resources/app/extensions/cursor-agent-exec/dist/main.js` | IDE agent shell library |
| `resources/app/product.json` | SHA-256 (base64, no padding) checksums of core files |

Before modifying a file, `nupatch` copies it to `<file>.bak`. `nupatch revert`
restores every file from its backup.

Cursor updates overwrite the patched files (and the CLI agent installs into a new
versioned directory). After an update, delete stale `.bak` files and re-run:

```
nupatch patch
nupatch status
```

Structural discovery handles releases that only rename identifiers automatically.

---

## Commands

```
nupatch patch              # patch both CLI + IDE
nupatch patch --cli-only   # CLI agent only
nupatch patch --ide-only   # IDE agent only
nupatch patch --dry-run    # preview every change without writing
nupatch status             # show patch state + checksum integrity
nupatch verify             # check product.json checksums against disk
nupatch fix-checksums      # recompute product.json checksums
nupatch revert             # restore all files from backups
```

## After patching

- **CLI**: Nushell is auto-detected from PATH — no `$env:SHELL` needed.
- **IDE**: fully quit and relaunch Cursor (not just "Reload Window"); check the
  system tray for lingering Cursor processes. Set
  `terminal.integrated.defaultProfile.windows` to your Nushell profile, using a
  literal `nu.exe` path (VS Code `${env:…}` variables don't always resolve here).
