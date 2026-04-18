/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-10 09:19:05
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:46:12
 * @FilePath: /udx710-backend/backend/src/ota.rs
 * @Description:
 *
 * Copyright (c) 2025 by 1orz, All Rights Reserved.
 */
//! OTA update module.
//!
//! Handles OTA upload, validation, application, and slot confirmation.

use crate::models::{OtaMeta, OtaStatusResponse, OtaUploadResponse, OtaValidation};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// OTA related paths
const OTA_RUNTIME_DIR: &str = "/home/root/ota";
const OTA_SLOT_ROOT: &str = "/home/root/ota/slots";
const OTA_STATE_FILE: &str = "/home/root/ota/state.env";
const OTA_STAGING_DIR: &str = "/tmp/ota_staging";
const OTA_BINARY_PATH: &str = "/home/root/udx710";
const OTA_WWW_PATH: &str = "/home/root/www";
const OTA_PORT: &str = "80";
const OTA_SLOT_A: &str = "slot-a";
const OTA_SLOT_B: &str = "slot-b";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtaSlot {
    Legacy,
    SlotA,
    SlotB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtaBootState {
    Stable,
    Trial,
}

#[derive(Debug, Clone)]
struct OtaRuntimeState {
    active_slot: OtaSlot,
    fallback_slot: Option<OtaSlot>,
    boot_state: OtaBootState,
    trial_attempts: u8,
}

/// Current version information (injected at compile time)
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Get current commit (from env or fallback)
pub fn get_current_commit() -> String {
    option_env!("GIT_COMMIT").unwrap_or("unknown").to_string()
}

/// Get OTA status
pub fn get_ota_status() -> OtaStatusResponse {
    let pending_meta = read_pending_meta().ok().flatten();
    let runtime_state = read_runtime_state().unwrap_or_else(|_| default_runtime_state());
    let pending_update = pending_meta.is_some() || matches!(runtime_state.boot_state, OtaBootState::Trial);

    OtaStatusResponse {
        current_version: CURRENT_VERSION.to_string(),
        current_commit: get_current_commit(),
        pending_update,
        pending_meta,
    }
}

fn default_runtime_state() -> OtaRuntimeState {
    OtaRuntimeState {
        active_slot: OtaSlot::Legacy,
        fallback_slot: None,
        boot_state: OtaBootState::Stable,
        trial_attempts: 0,
    }
}

fn slot_name(slot: OtaSlot) -> &'static str {
    match slot {
        OtaSlot::Legacy => "legacy",
        OtaSlot::SlotA => OTA_SLOT_A,
        OtaSlot::SlotB => OTA_SLOT_B,
    }
}

fn slot_from_name(value: &str) -> Option<OtaSlot> {
    match value.trim() {
        "legacy" => Some(OtaSlot::Legacy),
        OTA_SLOT_A => Some(OtaSlot::SlotA),
        OTA_SLOT_B => Some(OtaSlot::SlotB),
        _ => None,
    }
}

fn slot_directory(slot: OtaSlot) -> Option<PathBuf> {
    match slot {
        OtaSlot::Legacy => None,
        OtaSlot::SlotA => Some(PathBuf::from(OTA_SLOT_ROOT).join(OTA_SLOT_A)),
        OtaSlot::SlotB => Some(PathBuf::from(OTA_SLOT_ROOT).join(OTA_SLOT_B)),
    }
}

fn slot_binary_path(slot: OtaSlot) -> PathBuf {
    match slot {
        OtaSlot::Legacy => PathBuf::from(OTA_BINARY_PATH),
        OtaSlot::SlotA | OtaSlot::SlotB => slot_directory(slot)
            .unwrap_or_else(|| PathBuf::from(OTA_SLOT_ROOT))
            .join("udx710"),
    }
}

fn slot_meta_path(slot: OtaSlot) -> Option<PathBuf> {
    slot_directory(slot).map(|dir| dir.join("meta.json"))
}

fn detect_current_slot() -> OtaSlot {
    if let Ok(slot_name) = std::env::var("UDX710_ACTIVE_SLOT") {
        if let Some(slot) = slot_from_name(&slot_name) {
            return slot;
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        let slot_root = Path::new(OTA_SLOT_ROOT);
        if let Ok(relative) = exe_path.strip_prefix(slot_root) {
            if let Some(slot_component) = relative.components().next() {
                if let Some(slot) = slot_component.as_os_str().to_str().and_then(slot_from_name) {
                    return slot;
                }
            }
        }
    }

    OtaSlot::Legacy
}

fn next_slot(current_slot: OtaSlot) -> OtaSlot {
    match current_slot {
        OtaSlot::Legacy | OtaSlot::SlotB => OtaSlot::SlotA,
        OtaSlot::SlotA => OtaSlot::SlotB,
    }
}

fn parse_runtime_state(content: &str) -> OtaRuntimeState {
    let mut values = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let active_slot = values
        .get("ACTIVE_SLOT")
        .and_then(|value| slot_from_name(value))
        .unwrap_or(OtaSlot::Legacy);
    let fallback_slot = values
        .get("FALLBACK_SLOT")
        .and_then(|value| slot_from_name(value));
    let boot_state = match values.get("BOOT_STATE").map(|value| value.as_str()) {
        Some("trial") => OtaBootState::Trial,
        _ => OtaBootState::Stable,
    };
    let trial_attempts = values
        .get("TRIAL_ATTEMPTS")
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0);

    OtaRuntimeState {
        active_slot,
        fallback_slot,
        boot_state,
        trial_attempts,
    }
}

fn read_runtime_state() -> Result<OtaRuntimeState, String> {
    match fs::read_to_string(OTA_STATE_FILE) {
        Ok(content) => Ok(parse_runtime_state(&content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_runtime_state()),
        Err(e) => Err(format!("Failed to read OTA state file: {}", e)),
    }
}

fn write_runtime_state(state: &OtaRuntimeState) -> Result<(), String> {
    fs::create_dir_all(OTA_RUNTIME_DIR)
        .map_err(|e| format!("Failed to create OTA runtime directory: {}", e))?;

    let content = format!(
        "ACTIVE_SLOT={}\nBOOT_STATE={}\nPENDING_SLOT={}\nFALLBACK_SLOT={}\nTRIAL_ATTEMPTS={}\n",
        slot_name(state.active_slot),
        match state.boot_state {
            OtaBootState::Stable => "stable",
            OtaBootState::Trial => "trial",
        },
        match state.boot_state {
            OtaBootState::Trial => slot_name(state.active_slot),
            OtaBootState::Stable => "",
        },
        state.fallback_slot.map(slot_name).unwrap_or(""),
        state.trial_attempts
    );

    fs::write(OTA_STATE_FILE, content)
        .map_err(|e| format!("Failed to write OTA state file: {}", e))
}

fn read_meta_from_path(path: &Path) -> Option<OtaMeta> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn read_runtime_pending_meta() -> Option<OtaMeta> {
    let state = read_runtime_state().ok()?;
    if !matches!(state.boot_state, OtaBootState::Trial) {
        return None;
    }

    slot_meta_path(state.active_slot).and_then(|path| read_meta_from_path(&path))
}

/// Read pending OTA metadata from staging or the active trial slot.
fn read_pending_meta() -> Result<Option<OtaMeta>, String> {
    let meta_path = Path::new(OTA_STAGING_DIR).join("meta.json");
    let staging_meta = read_meta_from_path(&meta_path);
    if staging_meta.is_some() {
        return Ok(staging_meta);
    }

    Ok(read_runtime_pending_meta())
}

fn remove_slot_artifacts(slot: OtaSlot) -> Result<(), String> {
    match slot {
        OtaSlot::Legacy => {
            let _ = fs::remove_file(OTA_BINARY_PATH);
            let _ = fs::remove_dir_all(OTA_WWW_PATH);
            Ok(())
        }
        OtaSlot::SlotA | OtaSlot::SlotB => {
            if let Some(dir) = slot_directory(slot) {
                if dir.exists() {
                    fs::remove_dir_all(&dir).map_err(|e| {
                        format!("Failed to remove slot directory {}: {}", dir.display(), e)
                    })?;
                }
            }
            Ok(())
        }
    }
}

/// Handle uploaded OTA package (supports tar.gz and zip)
pub fn handle_ota_upload(data: &[u8]) -> Result<OtaUploadResponse, String> {
    let _ = fs::remove_dir_all(OTA_STAGING_DIR);
    fs::create_dir_all(OTA_STAGING_DIR)
        .map_err(|e| format!("Failed to create staging dir: {}", e))?;

    let is_zip = detect_zip_format(data);

    if is_zip {
        let zip_path = Path::new(OTA_STAGING_DIR).join("update.zip");
        let mut file = fs::File::create(&zip_path)
            .map_err(|e| format!("Failed to create zip file: {}", e))?;
        file.write_all(data)
            .map_err(|e| format!("Failed to write zip file: {}", e))?;

        let output = Command::new("unzip")
            .args(["-o", zip_path.to_str().unwrap_or(""), "-d", OTA_STAGING_DIR])
            .output()
            .map_err(|e| format!("Failed to extract zip: {}. Make sure 'unzip' is installed.", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to extract zip: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let _ = fs::remove_file(&zip_path);
    } else {
        let tar_path = Path::new(OTA_STAGING_DIR).join("update.tar.gz");
        let mut file = fs::File::create(&tar_path)
            .map_err(|e| format!("Failed to create tar file: {}", e))?;
        file.write_all(data)
            .map_err(|e| format!("Failed to write tar file: {}", e))?;

        let output = Command::new("tar")
            .args(["-xzf", tar_path.to_str().unwrap_or(""), "-C", OTA_STAGING_DIR])
            .output()
            .map_err(|e| format!("Failed to extract tar: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "Failed to extract tar: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let _ = fs::remove_file(&tar_path);
    }

    fix_file_permissions(OTA_STAGING_DIR)?;

    let meta_path = Path::new(OTA_STAGING_DIR).join("meta.json");
    let meta_content = fs::read_to_string(&meta_path)
        .map_err(|_| "meta.json not found in OTA package".to_string())?;

    let meta: OtaMeta = serde_json::from_str(&meta_content)
        .map_err(|e| format!("Invalid meta.json: {}", e))?;

    let validation = validate_ota_package(&meta)?;

    Ok(OtaUploadResponse { meta, validation })
}

/// Validate OTA package.
fn validate_ota_package(meta: &OtaMeta) -> Result<OtaValidation, String> {
    let binary_path = Path::new(OTA_STAGING_DIR).join("udx710");
    let www_path = Path::new(OTA_STAGING_DIR).join("www");

    if !binary_path.exists() {
        return Ok(OtaValidation {
            valid: false,
            is_newer: false,
            binary_md5_match: false,
            frontend_md5_match: false,
            arch_match: false,
            error: Some("Binary file not found in package".to_string()),
        });
    }

    if !www_path.exists() {
        return Ok(OtaValidation {
            valid: false,
            is_newer: false,
            binary_md5_match: false,
            frontend_md5_match: false,
            arch_match: false,
            error: Some("Frontend directory not found in package".to_string()),
        });
    }

    let binary_md5 = calculate_file_md5(&binary_path)?;
    let binary_md5_match = binary_md5 == meta.binary_md5;
    let frontend_md5 = calculate_directory_md5(&www_path)?;
    let frontend_md5_match = frontend_md5 == meta.frontend_md5;
    let arch_match = meta.arch == "aarch64-unknown-linux-musl";
    let is_newer = compare_versions(&meta.version, CURRENT_VERSION);

    let valid = binary_md5_match && frontend_md5_match && arch_match;

    let error = if !valid {
        let mut errors = Vec::new();
        if !binary_md5_match {
            errors.push(format!(
                "Binary MD5 mismatch: expected={}, actual={}",
                meta.binary_md5, binary_md5
            ));
        }
        if !frontend_md5_match {
            errors.push(format!(
                "Frontend MD5 mismatch: expected={}, actual={}",
                meta.frontend_md5, frontend_md5
            ));
        }
        if !arch_match {
            errors.push(format!(
                "Arch mismatch: expected=aarch64-unknown-linux-musl, actual={}",
                meta.arch
            ));
        }
        Some(errors.join("; "))
    } else {
        None
    };

    Ok(OtaValidation {
        valid,
        is_newer,
        binary_md5_match,
        frontend_md5_match,
        arch_match,
        error,
    })
}

fn calculate_file_md5(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|e| format!("Failed to open file {}: {}", path.display(), e))?;

    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    Ok(format!("{:x}", md5::compute(&contents)))
}

fn collect_directory_hashes(path: &Path, hashes: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(path)
        .map_err(|e| format!("Failed to read directory {}: {}", path.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            collect_directory_hashes(&entry_path, hashes)?;
        } else {
            hashes.push(calculate_file_md5(&entry_path)?);
        }
    }

    Ok(())
}

fn calculate_directory_md5(path: &Path) -> Result<String, String> {
    let mut hashes = Vec::new();
    collect_directory_hashes(path, &mut hashes)?;
    hashes.sort();

    let mut payload = hashes.join("\n");
    if !payload.is_empty() {
        payload.push('\n');
    }

    Ok(format!("{:x}", md5::compute(payload.as_bytes())))
}

/// Compare version numbers (return true if v1 > v2)
fn compare_versions(v1: &str, v2: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> {
        v.split('.')
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let v1_parts = parse(v1);
    let v2_parts = parse(v2);

    for i in 0..std::cmp::max(v1_parts.len(), v2_parts.len()) {
        let p1 = v1_parts.get(i).unwrap_or(&0);
        let p2 = v2_parts.get(i).unwrap_or(&0);
        if p1 > p2 {
            return true;
        } else if p1 < p2 {
            return false;
        }
    }
    false
}

/// Apply OTA update.
pub fn apply_ota_update(restart_now: bool) -> Result<String, String> {
    let meta = read_pending_meta()?
        .ok_or_else(|| "No pending update".to_string())?;
    let validation = validate_ota_package(&meta)?;
    if !validation.valid {
        return Err(validation
            .error
            .unwrap_or_else(|| "OTA package validation failed".to_string()));
    }

    let current_slot = detect_current_slot();
    let target_slot = next_slot(current_slot);
    let fallback_slot = current_slot;
    let target_dir = slot_directory(target_slot)
        .ok_or_else(|| "Failed to resolve target slot directory".to_string())?;
    let target_dir_string = target_dir
        .to_str()
        .ok_or_else(|| "Target slot directory contains invalid UTF-8".to_string())?
        .to_string();

    let _ = remove_slot_artifacts(target_slot);

    copy_dir_recursive(OTA_STAGING_DIR, &target_dir_string).map_err(|err| {
        let _ = remove_slot_artifacts(target_slot);
        err
    })?;
    fix_file_permissions(&target_dir_string).map_err(|err| {
        let _ = remove_slot_artifacts(target_slot);
        err
    })?;

    let state = OtaRuntimeState {
        active_slot: target_slot,
        fallback_slot: Some(fallback_slot),
        boot_state: OtaBootState::Trial,
        trial_attempts: 0,
    };

    write_runtime_state(&state).map_err(|err| {
        let _ = remove_slot_artifacts(target_slot);
        err
    })?;

    let _ = fs::remove_dir_all(OTA_STAGING_DIR);

    let message = format!(
        "Update to version {} staged in {}",
        meta.version,
        slot_name(target_slot)
    );

    if restart_now {
        let target_binary = slot_binary_path(target_slot);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            if Command::new(&target_binary)
                .args(["-p", OTA_PORT])
                .env("UDX710_ACTIVE_SLOT", slot_name(target_slot))
                .env("UDX710_FALLBACK_SLOT", slot_name(fallback_slot))
                .env("UDX710_OTA_STATE_FILE", OTA_STATE_FILE)
                .spawn()
                .is_ok()
            {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            }
        });
    }

    Ok(message)
}

/// Confirm that the current boot is healthy and clean up the previous slot.
pub fn confirm_boot_if_pending() -> Result<bool, String> {
    let mut state = read_runtime_state()?;
    if !matches!(state.boot_state, OtaBootState::Trial) {
        return Ok(false);
    }

    let current_slot = detect_current_slot();

    if current_slot == state.active_slot {
        if let Some(previous_slot) = state.fallback_slot {
            if !matches!(previous_slot, OtaSlot::Legacy) {
                let _ = remove_slot_artifacts(previous_slot);
            }
        }
        state.boot_state = OtaBootState::Stable;
        state.fallback_slot = None;
        state.trial_attempts = 0;
        write_runtime_state(&state)?;
        return Ok(true);
    }

    if Some(current_slot) == state.fallback_slot {
        let _ = remove_slot_artifacts(state.active_slot);
        state.active_slot = current_slot;
        state.boot_state = OtaBootState::Stable;
        state.fallback_slot = None;
        state.trial_attempts = 0;
        write_runtime_state(&state)?;
        return Ok(true);
    }

    Ok(false)
}

/// Recursive directory copy
fn copy_dir_recursive(src: &str, dst: &str) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create dir {}: {}", dst, e))?;

    let entries = fs::read_dir(src)
        .map_err(|e| format!("Failed to read src dir {}: {}", src, e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = Path::new(dst).join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(
                src_path.to_str().unwrap_or(""),
                dst_path.to_str().unwrap_or(""),
            )?;
        } else {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("Failed to copy file {}: {}", src_path.display(), e))?;
        }
    }

    Ok(())
}

/// Cancel pending update.
pub fn cancel_pending_update() -> Result<(), String> {
    let _ = fs::remove_dir_all(OTA_STAGING_DIR);

    let state = read_runtime_state()?;
    if matches!(state.boot_state, OtaBootState::Trial) {
        let reverted_slot = state.fallback_slot.unwrap_or(OtaSlot::Legacy);
        let reverted_state = OtaRuntimeState {
            active_slot: reverted_slot,
            fallback_slot: None,
            boot_state: OtaBootState::Stable,
            trial_attempts: 0,
        };
        write_runtime_state(&reverted_state)?;
    }

    Ok(())
}

/// Detect ZIP format by magic bytes.
fn detect_zip_format(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }

    data[0] == 0x50 && data[1] == 0x4B && data[2] == 0x03 && data[3] == 0x04
}

/// Fix file permissions under a root path.
fn fix_file_permissions(root: &str) -> Result<(), String> {
    let binary_path = format!("{}/udx710", root);
    let www_path = format!("{}/www", root);

    if Path::new(&binary_path).exists() {
        Command::new("chmod")
            .args(["755", &binary_path])
            .output()
            .map_err(|e| format!("Failed to chmod binary {}: {}", binary_path, e))?;
    }

    if Path::new(&www_path).exists() {
        let _ = Command::new("find")
            .args([&www_path, "-type", "d", "-exec", "chmod", "755", "{}", "+"])
            .output();

        let _ = Command::new("find")
            .args([&www_path, "-type", "f", "-exec", "chmod", "644", "{}", "+"])
            .output();
    }

    Ok(())
}
