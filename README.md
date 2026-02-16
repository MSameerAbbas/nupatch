# nupatch

Patch [Cursor](https://cursor.com) to use [Nushell](https://www.nushell.sh/) instead of PowerShell on Windows.

## Why

Cursor's CLI and IDE agents have several bugs that prevent Nushell from working:

1. `detectShellType()` has no `includes("nu")` check, and the PowerShell fallback on Windows is unreachable dead code for any shell placed after it
2. The shell resolution function ignores the user's configured terminal (`userTerminalHint`)
3. `getShellExecutablePath()` returns `/bin/sh` on Windows for unrecognized shell types
4. No system-level PATH detection for nushell — only checks for `pwsh`/`powershell`/`zsh`/`bash`

The `NaiveTerminalExecutor` (which runs `shell -c "command"`) already exists in Cursor and works perfectly with Nushell. It just needs to be wired up correctly.

## What it does

**CLI agent** (3 patches):
- Hint-level nushell detection (`includes("nu")`) before the PowerShell check
- System-level PATH detection (`commandExists("nu")`) as a fallback
- `case ShellType.Naive` in the executor factory with `findActualExecutable("nu")` shell resolution

**IDE agent** (4 patches):
- Same hint-level and system-level nushell detection
- `userTerminalHint` wired into the shell resolution function
- `case ShellType.Naive` in `getShellExecutablePath()` with PATH-based nu discovery, plus a Windows-safe `default:` fallback

**Integrity chain**: Updates SHA-256 hashes in `extensionHostProcess.js` and `product.json` so Cursor doesn't flag the modification.

All patches use regex-based pattern discovery to find minified variable names dynamically, so they should survive Cursor updates that only rename variables.

See [nushell-patch-guide.md](nushell-patch-guide.md) for the full technical deep-dive.

## Requirements

- [Rust](https://rustup.rs/) toolchain (edition 2024)
- [Nushell](https://www.nushell.sh/) installed and on PATH
- Cursor installed (Windows)

## Install

```
cargo install --path .
```

Or build without installing:

```
cargo build --release
# Binary at target/release/nupatch.exe
```

## Usage

```
nupatch patch              # patch both CLI + IDE agents
nupatch patch --cli-only   # patch CLI agent only
nupatch patch --ide-only   # patch IDE agent only
nupatch patch --dry-run    # show what would change without modifying files
nupatch status             # check current patch state and integrity
nupatch revert             # restore all files from backups
```

## After patching

**CLI**: Nushell is auto-detected from PATH. No `$env:SHELL` needed.

**IDE**: Full quit and relaunch Cursor (not just Reload Window). Check the system tray for lingering Cursor processes.

**Recommended `settings.json`** (Cursor):

```json
{
  "terminal.integrated.defaultProfile.windows": "Nushell",
  "terminal.integrated.profiles.windows": {
    "Nushell": {
      "path": "C:\\Users\\<you>\\.cargo\\bin\\nu.exe"
    }
  }
}
```

Use a literal path — VS Code `${env:VARNAME}` variables may not resolve correctly in all contexts.

## Re-applying after Cursor updates

Cursor updates overwrite patched files. Delete stale `.bak` files, then re-run:

```
nupatch patch
nupatch status
```

## Disclaimer

This tool modifies local Cursor installation files. Use at your own risk. Not affiliated with or endorsed by Anysphere, Inc. or the Cursor project.

## License

[MIT](LICENSE)
