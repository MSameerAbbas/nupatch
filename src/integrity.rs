//! SHA-256 hashing, backup/restore, integrity chain updates.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::patch::{PatchResult, StepResult};

// ---------------------------------------------------------------------------
//  Helpers
// ---------------------------------------------------------------------------

/// Convert 2-space indentation to tab indentation, only in leading whitespace.
/// Avoids corrupting string values that might contain double spaces.
fn tab_indent(json: &str) -> String {
    json.lines()
        .map(|line| {
            let trimmed = line.trim_start_matches("  ");
            let depth = (line.len() - trimmed.len()) / 2;
            format!("{}{}", "\t".repeat(depth), trimmed)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
//  Hashing
// ---------------------------------------------------------------------------

/// SHA-256 hex digest of a file.
pub fn sha256_hex(path: &Path) -> Result<String> {
    let hash = Sha256::digest(fs::read(path)?);
    let mut hex = String::with_capacity(hash.len() * 2);
    for b in hash {
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

/// SHA-256 base64 digest with trailing `=` stripped.
pub fn sha256_base64_stripped(path: &Path) -> Result<String> {
    let hash = Sha256::digest(fs::read(path)?);
    Ok(STANDARD.encode(hash).trim_end_matches('=').to_string())
}

// ---------------------------------------------------------------------------
//  Backup / restore
// ---------------------------------------------------------------------------

/// Create a `.bak` copy if one doesn't already exist.
pub fn backup(filepath: &Path) -> std::io::Result<PathBuf> {
    let bak = bak_path(filepath);
    if !bak.exists() {
        fs::copy(filepath, &bak)?;
    }
    Ok(bak)
}

/// Restore a file from its `.bak` copy. Returns true on success.
pub fn restore_from_backup(filepath: &Path) -> std::io::Result<bool> {
    let bak = bak_path(filepath);
    if bak.exists() {
        fs::copy(&bak, filepath)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get the `.bak` path for a file.
/// Returns the path unchanged if `file_name()` is `None` (e.g. root path).
pub fn bak_path(filepath: &Path) -> PathBuf {
    match filepath.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push(".bak");
            filepath.with_file_name(name)
        }
        None => filepath.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
//  Update integrity chain
// ---------------------------------------------------------------------------

/// Recompute `product.json` checksums after patching, so Cursor's startup
/// `FileIntegrityService` does not flag the install as corrupt.
///
/// Returns a `PatchResult` directly (no `Result` wrapper) so callers handle a
/// single failure channel, matching the core patch functions. (Older Cursor
/// builds also embedded a `main.js` hash in `extensionHostProcess.js`; current
/// builds dropped it, so only the checksum layer needs maintaining.)
pub fn update_integrity(
    product_json: Option<&Path>,
    cursor_app: Option<&Path>,
    dry_run: bool,
) -> PatchResult {
    let mut steps: Vec<StepResult> = Vec::new();
    let fail = |steps: Vec<StepResult>| PatchResult { success: false, steps };

    let (Some(product_json), Some(cursor_app)) = (product_json, cursor_app) else {
        return fail(vec![StepResult::fail("Integrity", "Missing product.json / cursor app path")]);
    };

    if !dry_run
        && let Err(e) = backup(product_json)
    {
        steps.push(StepResult::fail("Product backup", format!("Failed to backup product.json: {e}")));
        return fail(steps);
    }

    let product_text = match fs::read_to_string(product_json) {
        Ok(t) => t,
        Err(e) => {
            steps.push(StepResult::fail("Product checksums", format!("Failed to read product.json: {e}")));
            return fail(steps);
        }
    };
    let mut product: Value = match serde_json::from_str(&product_text) {
        Ok(v) => v,
        Err(e) => {
            steps.push(StepResult::fail("Product checksums", format!("Failed to parse product.json: {e}")));
            return fail(steps);
        }
    };

    let checksums = match product.get_mut("checksums").and_then(|v| v.as_object_mut()) {
        Some(c) => c,
        None => {
            steps.push(StepResult::fail("Product checksums", "No checksums section in product.json"));
            return fail(steps);
        }
    };

    let mut changed = 0u32;
    let entries: Vec<(String, String)> = checksums
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect();

    for (rel_path, old_hash) in &entries {
        let full_path = cursor_app.join("out").join(rel_path);
        if !full_path.is_file() {
            continue;
        }
        let new_hash = match sha256_base64_stripped(&full_path) {
            Ok(h) => h,
            Err(e) => {
                steps.push(StepResult::fail("Product checksums", format!("Failed to hash {rel_path}: {e}")));
                return fail(steps);
            }
        };
        if old_hash != &new_hash {
            checksums.insert(rel_path.clone(), Value::String(new_hash));
            changed += 1;
        }
    }

    if changed > 0 && !dry_run {
        let out = match serde_json::to_string_pretty(&product) {
            Ok(s) => s,
            Err(e) => {
                steps.push(StepResult::fail("Product checksums", format!("Failed to serialize product.json: {e}")));
                return fail(steps);
            }
        };
        // Match original tab indentation
        let out = tab_indent(&out);
        if let Err(e) = fs::write(product_json, out) {
            steps.push(StepResult::fail("Product checksums", format!("Failed to write product.json: {e}")));
            return fail(steps);
        }
    }

    steps.push(StepResult::ok("Product checksums", format!("Updated {changed} checksum(s) in product.json")));

    PatchResult {
        success: true,
        steps,
    }
}

/// Read and parse product.json, returning the parsed JSON value and the
/// checksums map. Shared preamble for verify/fix/update operations.
fn load_product_checksums(product_json: &Path) -> Result<(Value, serde_json::Map<String, Value>)> {
    let product_text = fs::read_to_string(product_json)?;
    let product: Value = serde_json::from_str(&product_text)?;
    let checksums = product
        .get("checksums")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    Ok((product, checksums))
}

/// Check whether all product.json checksums match the files on disk.
/// Returns `None` if product.json cannot be read or lacks a checksums section.
pub fn checksums_all_match(product_json: &Path, cursor_app: &Path) -> Option<bool> {
    let (_product, checksums) = load_product_checksums(product_json).ok()?;
    if checksums.is_empty() {
        return None;
    }
    for (rel_path, expected_val) in &checksums {
        let expected = expected_val.as_str().unwrap_or("");
        let full = cursor_app.join("out").join(rel_path);
        if full.is_file()
            && let Ok(actual) = sha256_base64_stripped(&full)
            && actual != expected
        {
            return Some(false);
        }
    }
    Some(true)
}

// ---------------------------------------------------------------------------
//  Verify checksums
// ---------------------------------------------------------------------------

/// Single checksum verification entry.
pub struct VerifyEntry {
    pub rel_path: String,
    pub expected: String,
    pub actual: String,
    pub matches: bool,
    pub missing: bool,
}

/// Result of checksum verification.
pub struct VerifyResult {
    pub entries: Vec<VerifyEntry>,
    pub all_match: bool,
}

/// Verify every checksum in product.json against files on disk.
pub fn verify_checksums(
    product_json: &Path,
    cursor_app: &Path,
) -> Result<VerifyResult> {
    let (_product, checksums) = load_product_checksums(product_json)?;

    let mut result = VerifyResult {
        entries: vec![],
        all_match: true,
    };

    for (rel_path, expected_val) in &checksums {
        let expected = expected_val.as_str().unwrap_or("").to_string();
        let full_path = cursor_app.join("out").join(rel_path);

        if !full_path.is_file() {
            result.entries.push(VerifyEntry {
                rel_path: rel_path.clone(),
                expected,
                actual: String::new(),
                matches: false,
                missing: true,
            });
            result.all_match = false;
            continue;
        }

        let actual = sha256_base64_stripped(&full_path)?;
        let matches = actual == expected;
        if !matches {
            result.all_match = false;
        }
        result.entries.push(VerifyEntry {
            rel_path: rel_path.clone(),
            expected,
            actual,
            matches,
            missing: false,
        });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
//  Fix checksums
// ---------------------------------------------------------------------------

/// Status of a single checksum fix.
pub enum FixStatus {
    Ok,
    Updated,
    Missing,
}

/// Single checksum fix entry.
pub struct FixEntry {
    pub rel_path: String,
    pub status: FixStatus,
}

/// Result of checksum fix operation.
pub struct FixChecksumsResult {
    pub entries: Vec<FixEntry>,
    pub changed_count: u32,
}

/// Recompute and write correct checksums into product.json.
pub fn fix_checksums(
    product_json: &Path,
    cursor_app: &Path,
) -> Result<FixChecksumsResult> {
    let (mut product, _) = load_product_checksums(product_json)?;

    let checksums = match product.get_mut("checksums").and_then(|v| v.as_object_mut()) {
        Some(c) => c,
        None => {
            return Ok(FixChecksumsResult {
                entries: vec![],
                changed_count: 0,
            });
        }
    };

    let mut result = FixChecksumsResult {
        entries: vec![],
        changed_count: 0,
    };

    let keys: Vec<(String, String)> = checksums
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect();

    for (rel_path, old_hash) in &keys {
        let full_path = cursor_app.join("out").join(rel_path);

        if !full_path.is_file() {
            result.entries.push(FixEntry {
                rel_path: rel_path.clone(),
                status: FixStatus::Missing,
            });
            continue;
        }

        let new_hash = sha256_base64_stripped(&full_path)?;
        if old_hash == &new_hash {
            result.entries.push(FixEntry {
                rel_path: rel_path.clone(),
                status: FixStatus::Ok,
            });
        } else {
            checksums.insert(rel_path.clone(), Value::String(new_hash));
            result.entries.push(FixEntry {
                rel_path: rel_path.clone(),
                status: FixStatus::Updated,
            });
            result.changed_count += 1;
        }
    }

    if result.changed_count > 0 {
        let out = serde_json::to_string_pretty(&product)?;
        let out = tab_indent(&out);
        fs::write(product_json, out)?;
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
//  Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn sha256_hex_known_vector() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("abc.txt");
        write(&f, "abc");
        assert_eq!(
            sha256_hex(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_base64_is_stripped_and_43_chars() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("abc.txt");
        write(&f, "abc");
        let b64 = sha256_base64_stripped(&f).unwrap();
        assert_eq!(b64.len(), 43);
        assert!(!b64.ends_with('='));
    }

    #[test]
    fn bak_path_appends_suffix() {
        assert_eq!(bak_path(Path::new("a/b/main.js")), Path::new("a/b/main.js.bak"));
    }

    #[test]
    fn backup_and_restore_roundtrip() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("file.txt");
        write(&f, "original");
        backup(&f).unwrap();
        write(&f, "modified");
        assert!(restore_from_backup(&f).unwrap());
        assert_eq!(fs::read_to_string(&f).unwrap(), "original");
    }

    #[test]
    fn restore_without_backup_returns_false() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("file.txt");
        write(&f, "x");
        assert!(!restore_from_backup(&f).unwrap());
    }

    #[test]
    fn tab_indent_converts_leading_spaces_only() {
        let input = "{\n  \"a\": \"two  spaces\",\n    \"b\": 1\n}";
        let out = tab_indent(input);
        assert_eq!(out, "{\n\t\"a\": \"two  spaces\",\n\t\t\"b\": 1\n}");
    }

    /// Build a Cursor-like tree with `out/<file>` and a `product.json` whose
    /// checksum for that file is stale, then exercise verify + fix.
    fn checksum_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempdir().unwrap();
        let app = dir.path().to_path_buf();
        let out = app.join("out").join("vs");
        fs::create_dir_all(&out).unwrap();
        write(&out.join("core.js"), "hello world");
        let product = app.join("product.json");
        write(&product, "{\n\t\"checksums\": {\n\t\t\"vs/core.js\": \"stale\"\n\t}\n}");
        (dir, product, app)
    }

    #[test]
    fn verify_reports_mismatch_then_fix_repairs_it() {
        let (_dir, product, app) = checksum_fixture();

        let before = verify_checksums(&product, &app).unwrap();
        assert!(!before.all_match);
        assert_eq!(before.entries.len(), 1);
        assert!(!before.entries[0].matches);

        let fixed = fix_checksums(&product, &app).unwrap();
        assert_eq!(fixed.changed_count, 1);
        assert!(matches!(fixed.entries[0].status, FixStatus::Updated));

        let after = verify_checksums(&product, &app).unwrap();
        assert!(after.all_match);
    }

    #[test]
    fn verify_flags_missing_file() {
        let (_dir, product, app) = checksum_fixture();
        fs::remove_file(app.join("out").join("vs").join("core.js")).unwrap();
        let result = verify_checksums(&product, &app).unwrap();
        assert!(result.entries[0].missing);
        assert!(!result.all_match);
    }
}

