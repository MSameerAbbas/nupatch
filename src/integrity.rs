//! SHA-256 hashing, backup/restore, integrity chain updates.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use color_eyre::eyre::{self, WrapErr};

use crate::core::{PatchResult, StepResult};
use crate::util::lazy_re;

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
pub fn sha256_hex(path: &Path) -> eyre::Result<String> {
    let data = fs::read(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let hash = Sha256::digest(&data);
    Ok(format!("{:x}", hash))
}

/// SHA-256 base64 digest with trailing `=` stripped.
pub fn sha256_base64_stripped(path: &Path) -> eyre::Result<String> {
    let data = fs::read(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let hash = Sha256::digest(&data);
    Ok(STANDARD.encode(hash).trim_end_matches('=').to_string())
}

// ---------------------------------------------------------------------------
//  Backup / restore
// ---------------------------------------------------------------------------

/// Create a `.bak` copy if one doesn't already exist.
pub fn backup(filepath: &Path) -> Result<PathBuf, std::io::Error> {
    let bak = bak_path(filepath);
    if !bak.exists() {
        fs::copy(filepath, &bak)?;
    }
    Ok(bak)
}

/// Restore a file from its `.bak` copy. Returns true on success.
pub fn restore_from_backup(filepath: &Path) -> Result<bool, std::io::Error> {
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
//  Update integrity hashes
// ---------------------------------------------------------------------------

/// Update the SHA-256 integrity chain after patching the IDE agent.
///
/// Returns a `PatchResult` directly (no `eyre::Result` wrapper) so callers
/// handle a single failure channel, matching the pattern used by the core
/// patch functions.
pub fn update_integrity(
    ide_main: &Path,
    ehp: Option<&Path>,
    product_json: Option<&Path>,
    cursor_app: Option<&Path>,
    dry_run: bool,
) -> PatchResult {
    let mut steps: Vec<StepResult> = Vec::new();

    let fail = |steps: Vec<StepResult>| PatchResult { success: false, steps };

    let (Some(ehp), Some(product_json), Some(cursor_app)) = (ehp, product_json, cursor_app)
    else {
        return fail(vec![StepResult::fail("Integrity", "Missing EHP / product.json / cursor app path")]);
    };

    // Step 1: compute new hash of patched main.js
    let new_main_hash = match sha256_hex(ide_main) {
        Ok(h) => h,
        Err(e) => {
            return fail(vec![StepResult::fail("Compute hash", format!("Failed to hash main.js: {e}"))]);
        }
    };
    steps.push(StepResult::ok("Compute hash", format!("main.js SHA-256: {}...", &new_main_hash[..16])));

    // Step 2: update hash in extensionHostProcess.js
    if !dry_run {
        if let Err(e) = backup(ehp) {
            return fail(vec![StepResult::fail("EHP backup", format!("Failed to backup EHP: {e}"))]);
        }
        if let Err(e) = restore_from_backup(ehp) {
            return fail(vec![StepResult::fail("EHP restore", format!("Failed to restore EHP: {e}"))]);
        }
    }

    let mut ehp_code = match fs::read_to_string(ehp) {
        Ok(c) => c,
        Err(e) => {
            return fail(vec![StepResult::fail("EHP read", format!("Failed to read EHP: {e}"))]);
        }
    };

    let hash_re = lazy_re!(
        r#"(cursor-agent-exec[^}]*dist:\{[^}]*"main\.js":")([a-f0-9]{64})(")"#
    );

    if let Some(caps) = hash_re.captures(&ehp_code).ok().flatten() {
        let old_hash = caps.get(2).unwrap().as_str();
        ehp_code = ehp_code.replacen(old_hash, &new_main_hash, 1);
        steps.push(StepResult::ok("EHP hash", "Replaced hash in extensionHostProcess.js"));
    } else {
        // Fallback: compute old hash from backup
        let bak = bak_path(ide_main);
        if bak.exists() {
            let old_hash = match sha256_hex(&bak) {
                Ok(h) => h,
                Err(e) => {
                    steps.push(StepResult::fail("EHP hash", format!("Failed to hash backup: {e}")));
                    return fail(steps);
                }
            };
            let count = ehp_code.matches(&old_hash).count();
            if count == 1 {
                ehp_code = ehp_code.replacen(&old_hash, &new_main_hash, 1);
                steps.push(StepResult::ok("EHP hash", "Replaced hash via backup comparison"));
            } else {
                steps.push(StepResult::fail("EHP hash", format!("Old hash found {count} time(s) (expected 1)")));
                return fail(steps);
            }
        } else {
            steps.push(StepResult::fail("EHP hash", "Cannot find hash map pattern or backup file"));
            return fail(steps);
        }
    }

    if !dry_run
        && let Err(e) = fs::write(ehp, &ehp_code)
    {
        steps.push(StepResult::fail("EHP write", format!("Failed to write EHP: {e}")));
        return fail(steps);
    }

    // Step 3: update product.json checksums
    if !dry_run {
        if let Err(e) = backup(product_json) {
            steps.push(StepResult::fail("Product backup", format!("Failed to backup product.json: {e}")));
            return fail(steps);
        }
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
fn load_product_checksums(product_json: &Path) -> eyre::Result<(Value, serde_json::Map<String, Value>)> {
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
) -> eyre::Result<VerifyResult> {
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
) -> eyre::Result<FixChecksumsResult> {
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
