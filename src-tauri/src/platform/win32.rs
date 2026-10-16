//! Windows-specific helpers, written against the raw Win32 ABI.
//!
//! The bindings are declared by hand rather than pulled from a binding crate so the set of OS
//! entry points Diskwise uses stays explicit and reviewable: volume metadata, on-disk size,
//! and Recycle Bin query. File Explorer is the only executable launched, by fixed name and never
//! through a shell command. The webview can only ask for a narrowly-scoped native command.
//!
//! Every function is `unsafe`-free at the boundary: raw pointers are created and dropped inside
//! these functions and never escape.

use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/* ------------------------------- declarations ------------------------------- */

#[link(name = "kernel32")]
extern "system" {
    fn GetLogicalDrives() -> u32;
    fn GetWindowsDirectoryW(lpBuffer: *mut u16, uSize: u32) -> u32;
    fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
    fn GetDiskFreeSpaceExW(
        lpDirectoryName: *const u16,
        lpFreeBytesAvailableToCaller: *mut u64,
        lpTotalNumberOfBytes: *mut u64,
        lpTotalNumberOfFreeBytes: *mut u64,
    ) -> i32;
    fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: u32,
        lpVolumeSerialNumber: *mut u32,
        lpMaximumComponentLength: *mut u32,
        lpFileSystemFlags: *mut u32,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: u32,
    ) -> i32;
    fn GetCompressedFileSizeW(lpFileName: *const u16, lpFileSizeHigh: *mut u32) -> u32;
    fn GetLastError() -> u32;
}

#[link(name = "shell32")]
extern "system" {
    fn SHQueryRecycleBinW(pszRootPath: *const u16, pSHQueryRBInfo: *mut ShQueryRbInfo) -> i32;
}

/// `SHQUERYRBINFOW`.
#[repr(C)]
#[derive(Default)]
struct ShQueryRbInfo {
    cb_size: u32,
    i64_size: u64,
    i64_num_items: u64,
}

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

const INVALID_FILE_SIZE: u32 = 0xFFFF_FFFF;

/* --------------------------------- helpers --------------------------------- */

/// Wide, NUL-terminated copy of a path. Windows APIs want this form.
pub fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn wide_str(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error() -> u32 {
    // SAFETY: reading the thread's last-error value has no preconditions.
    unsafe { GetLastError() }
}

/* -------------------------------- drive info -------------------------------- */

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveKind {
    Unknown,
    NoRootDir,
    Removable,
    Fixed,
    Network,
    Optical,
    RamDisk,
}

impl DriveKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriveKind::Unknown => "unknown",
            DriveKind::NoRootDir => "unknown",
            DriveKind::Removable => "removable",
            DriveKind::Fixed => "fixed",
            DriveKind::Network => "network",
            DriveKind::Optical => "optical",
            DriveKind::RamDisk => "ramdisk",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VolumeInfo {
    /// `C:`
    pub id: String,
    /// `C:\`
    pub root: String,
    pub kind: DriveKind,
    pub label: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// Volume is present but not ready (empty card reader, unformatted media).
    pub disconnected: bool,
    pub read_only: bool,
}

/// Enumerates every logical drive letter, including ones that are not ready.
pub fn list_volumes() -> Vec<VolumeInfo> {
    // SAFETY: `GetLogicalDrives` takes no arguments and cannot fail.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for index in 0..26u32 {
        if mask & (1u32 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        out.push(volume_info(&root));
    }
    out
}

fn volume_info(root: &str) -> VolumeInfo {
    let root_path = Path::new(root);
    let wide_root = wide(root_path);

    // SAFETY: `wide_root` is a valid NUL-terminated wide string that outlives the call.
    let drive_type = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
    let kind = match drive_type {
        DRIVE_UNKNOWN => DriveKind::Unknown,
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::Optical,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        DRIVE_NO_ROOT_DIR => DriveKind::NoRootDir,
        _ => DriveKind::Unknown,
    };

    let mut total: u64 = 0;
    let mut free: u64 = 0;
    // SAFETY: pointers are to live locals; the wide string outlives the call.
    let space_ok = unsafe {
        GetDiskFreeSpaceExW(
            wide_root.as_ptr(),
            std::ptr::null_mut(),
            &mut total as *mut u64,
            &mut free as *mut u64,
        )
    } != 0;

    let mut name_buf = [0u16; 260];
    let mut fs_buf = [0u16; 64];
    let mut serial: u32 = 0;
    let mut max_component: u32 = 0;
    let mut flags: u32 = 0;
    // SAFETY: buffers are sized correctly and live for the duration of the call.
    let info_ok = unsafe {
        GetVolumeInformationW(
            wide_root.as_ptr(),
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            &mut serial as *mut u32,
            &mut max_component as *mut u32,
            &mut flags as *mut u32,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    } != 0;

    let label = if info_ok {
        let end = name_buf
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(name_buf.len());
        String::from_utf16_lossy(&name_buf[..end])
            .trim()
            .to_string()
    } else {
        String::new()
    };

    let file_system = if info_ok {
        let end = fs_buf.iter().position(|c| *c == 0).unwrap_or(fs_buf.len());
        String::from_utf16_lossy(&fs_buf[..end]).trim().to_string()
    } else {
        String::new()
    };

    // FILE_READ_ONLY_VOLUME (0x0008_0000) / FILE_VOLUME_IS_COMPRESSED (0x0000_8000).
    let read_only = info_ok && (flags & 0x0008_0000) != 0;

    VolumeInfo {
        id: root.trim_end_matches('\\').to_string(),
        root: root.to_string(),
        kind,
        label,
        file_system,
        total_bytes: total,
        free_bytes: free,
        disconnected: !space_ok || !info_ok,
        read_only,
    }
}

/// Drive letter of a path, when the path is on a drive-rooted volume.
pub fn drive_letter(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().to_string();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(text[..2].to_uppercase());
    }
    None
}

pub fn is_system_drive(path: &Path) -> bool {
    static SYSTEM_DRIVE: OnceLock<Option<String>> = OnceLock::new();
    let system = SYSTEM_DRIVE.get_or_init(|| {
        std::env::var("SystemDrive")
            .ok()
            .or_else(|| std::env::var("SYSTEMDRIVE").ok())
    });
    match (system.as_deref(), drive_letter(path)) {
        (Some(system), Some(letter)) => system.eq_ignore_ascii_case(&letter),
        _ => false,
    }
}

/* ------------------------------- on-disk size ------------------------------- */

/// Bytes actually allocated on disk, including sparse and compressed files.
/// Returns `None` when the platform cannot tell us (non-NTFS, network share, …).
pub fn disk_size(path: &Path) -> Option<u64> {
    let wide_path = wide(path);
    let mut high: u32 = 0;
    // SAFETY: valid wide string; `high` points at a live local.
    let low = unsafe { GetCompressedFileSizeW(wide_path.as_ptr(), &mut high as *mut u32) };
    if low == INVALID_FILE_SIZE && last_error() != 0 {
        return None;
    }
    Some((high as u64) << 32 | low as u64)
}

/* -------------------------------- Recycle Bin -------------------------------- */

#[derive(Debug, Clone)]
pub struct RecycleBinInfo {
    pub item_count: u64,
    pub size_bytes: u64,
    pub supported: bool,
    pub detail: String,
}

/// Queries the shell Recycle Bin for a drive root (`C:\`) or the whole machine (`None`).
pub fn recycle_bin(drive_root: Option<&str>) -> RecycleBinInfo {
    let wide_root: Vec<u16> = match drive_root {
        Some(root) => wide(Path::new(root)),
        None => wide_str(""),
    };

    let mut info = ShQueryRbInfo {
        cb_size: std::mem::size_of::<ShQueryRbInfo>() as u32,
        i64_size: 0,
        i64_num_items: 0,
    };
    // SAFETY: `info` is correctly sized and lives for the duration of the call.
    let hr = unsafe { SHQueryRecycleBinW(wide_root.as_ptr(), &mut info as *mut ShQueryRbInfo) };
    if hr < 0 {
        return RecycleBinInfo {
            item_count: 0,
            size_bytes: 0,
            supported: false,
            detail: "Windows did not report the Recycle Bin contents for this volume.".to_string(),
        };
    }

    RecycleBinInfo {
        item_count: info.i64_num_items,
        size_bytes: info.i64_size,
        supported: true,
        detail: format!(
            "{} item(s), {} bytes reported by the Windows shell.",
            info.i64_num_items, info.i64_size
        ),
    }
}

/* --------------------------------- open path -------------------------------- */

fn explorer_executable() -> Result<PathBuf, String> {
    // Resolve the OS directory rather than searching the current directory/PATH for explorer.exe.
    let mut buffer = [0u16; 32_768];
    // SAFETY: the array is writable for `buffer.len()` UTF-16 code units.
    let length = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 || length >= buffer.len() as u32 {
        return Err("Could not locate the Windows directory.".to_string());
    }
    Ok(
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]))
            .join("explorer.exe"),
    )
}

/// Opens an ordinary folder using the OS-provided File Explorer executable. The native command
/// rejects files first; passing a path as an argument to Explorer cannot execute that path.
pub fn open_folder_in_explorer(path: &Path) -> Result<(), String> {
    std::process::Command::new(explorer_executable()?)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not start File Explorer: {error}"))
}

/// Reveals a path in File Explorer with the item selected.
pub fn reveal_in_explorer(path: &Path) -> Result<(), String> {
    let argument = format!("/select,{}", path.display());
    match std::process::Command::new(explorer_executable()?)
        .arg(&argument)
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("Could not start File Explorer: {error}")),
    }
}
