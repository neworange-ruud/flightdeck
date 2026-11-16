//! The Windows half of [`super::SystemProbe`] (`remote-control-cefv.12.5`):
//! listening TCP sockets with their processes, and each process's working
//! directory.
//!
//! - **Sockets**: `GetExtendedTcpTable` (listeners with their owning
//!   process), IPv4 and IPv6.
//! - **Working directory**: Windows has no call for another process's, so
//!   it is read where the process keeps it, as Process Explorer does: its
//!   PEB → `ProcessParameters` → `CurrentDirectory`. That needs the process
//!   to be this user's (`PROCESS_VM_READ`) and a 64-bit FlightDeck; a
//!   process it cannot read has no directory, and its ports are offered to
//!   no session. The directory is canonicalised, as the worktree it is
//!   compared with is, so case and `\\?\` agree.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

use super::Listener;

/// Every listening TCP socket, with its process's working directory.
pub fn listening() -> Vec<Listener> {
    let mut sockets =
        table::<MIB_TCPROW_OWNER_PID>(AF_INET as u32, |row| (row.dwOwningPid, row.dwLocalPort));
    sockets.extend(table::<MIB_TCP6ROW_OWNER_PID>(AF_INET6 as u32, |row| {
        (row.dwOwningPid, row.dwLocalPort)
    }));
    let mut cwds = std::collections::HashMap::new();
    sockets
        .into_iter()
        .filter_map(|(pid, port)| {
            // The port is in network order in the low 16 bits.
            let port = u16::from_be(port as u16);
            (port != 0).then_some((pid, port))
        })
        .map(|(pid, port)| Listener {
            port,
            cwd: cwds
                .entry(pid)
                .or_insert_with(|| working_directory(pid))
                .clone(),
        })
        .collect()
}

/// The listener rows of the TCP table for address family `family`, each
/// turned into (process, port) by `pick`.
fn table<Row>(family: u32, pick: impl Fn(&Row) -> (u32, u32)) -> Vec<(u32, u32)> {
    let mut size = 0u32;
    // SAFETY: the first call only reports the size needed; the second fills
    // a buffer of that size (u32s, so the rows are aligned), read as the
    // documented table: a count, then that many rows.
    unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if size == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u32; (size as usize).div_ceil(4) + 1];
        let status = GetExtendedTcpTable(
            buffer.as_mut_ptr().cast(),
            &mut size,
            0,
            family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if status != 0 {
            return Vec::new();
        }
        let count = buffer[0] as usize;
        let room = (buffer.len() - 1) * 4 / std::mem::size_of::<Row>();
        let rows =
            std::slice::from_raw_parts(buffer.as_ptr().add(1).cast::<Row>(), count.min(room));
        rows.iter().map(pick).collect()
    }
}

/// Process `pid`'s current directory, canonicalised; `None` when it cannot
/// be read.
fn working_directory(pid: u32) -> Option<PathBuf> {
    let raw = read_current_directory(pid)?;
    Some(std::fs::canonicalize(&raw).unwrap_or(raw))
}

/// An open process handle, closed when dropped.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: a handle OpenProcess returned, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

impl Process {
    /// `size_of::<T>()` bytes at `address` in the process.
    fn read<T: Copy + Default>(&self, address: usize) -> Option<T> {
        let mut value = T::default();
        let mut read = 0usize;
        // SAFETY: reads at most size_of::<T>() bytes into `value`; a bad
        // address makes the call fail, never touches our memory beyond it.
        let ok = unsafe {
            ReadProcessMemory(
                self.0,
                address as *const c_void,
                (&mut value as *mut T).cast(),
                std::mem::size_of::<T>(),
                &mut read,
            )
        };
        (ok != 0 && read == std::mem::size_of::<T>()).then_some(value)
    }
}

/// PEB → `ProcessParameters` → `CurrentDirectory.DosPath`, for a 64-bit
/// FlightDeck (the layout below is the 64-bit one).
#[cfg(target_pointer_width = "64")]
fn read_current_directory(pid: u32) -> Option<PathBuf> {
    /// `PEB.ProcessParameters`.
    const PROCESS_PARAMETERS: usize = 0x20;
    /// `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath`: a
    /// `UNICODE_STRING`, its length (bytes) then, at +8, its buffer.
    const CURRENT_DIRECTORY: usize = 0x38;
    // SAFETY: OpenProcess and NtQueryInformationProcess with valid
    // arguments; the handle is closed by `Process`.
    let process = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }
        Process(handle)
    };
    let mut info: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut length = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            ProcessBasicInformation,
            (&mut info as *mut PROCESS_BASIC_INFORMATION).cast(),
            std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut length,
        )
    };
    if status != 0 || info.PebBaseAddress.is_null() {
        return None;
    }
    let parameters: usize = process.read(info.PebBaseAddress as usize + PROCESS_PARAMETERS)?;
    let bytes: u16 = process.read(parameters + CURRENT_DIRECTORY)?;
    let buffer: usize = process.read(parameters + CURRENT_DIRECTORY + 8)?;
    if bytes == 0 || buffer == 0 {
        return None;
    }
    let mut wide = vec![0u16; usize::from(bytes) / 2];
    let mut read = 0usize;
    // SAFETY: reads exactly the string's bytes into a buffer of that size.
    let ok = unsafe {
        ReadProcessMemory(
            process.0,
            buffer as *const c_void,
            wide.as_mut_ptr().cast(),
            usize::from(bytes),
            &mut read,
        )
    };
    if ok == 0 || read != usize::from(bytes) {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(not(target_pointer_width = "64"))]
fn read_current_directory(_pid: u32) -> Option<PathBuf> {
    None
}
