//! The named pipe of the local interface on Windows
//! (`docs/m8-local-interface.md`): its name and the security descriptor
//! that lets only the current user open it. The HTTP part and the serve
//! loop are in `lz_client::local`.

use std::ffi::c_void;
use std::io;
use std::ptr;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Name of the pipe of the desktop app for the user with the SID `sid`
/// (as text, `S-1-5-21-…`): `\\.\pipe\lebenszeichen-<sid>-app`.
///
/// Pipe names are global on a machine, across sessions. With the SID in
/// the name each user who is signed in gets an interface of their own; a
/// fixed name would let only the first user's app serve. The SID is no
/// secret, so it does not keep another user from taking the name first:
/// that is what the first instance (`first_pipe_instance(true)`) is for,
/// which fails if the name exists, and then the app does not serve.
pub fn pipe_name(sid: &str) -> String {
    format!(r"\\.\pipe\lebenszeichen-{sid}-app")
}

/// SDDL of the pipe: a protected DACL (`P`, nothing inherited) with one
/// entry that gives the user `sid` full access (`GA`). Nobody else, not
/// Everyone, not anonymous, gets in. Full access rather than read and
/// write because the app needs `FILE_CREATE_PIPE_INSTANCE` on its own pipe
/// to create the further instances. The owner is set to the user as well
/// (`O:`), also when the app runs elevated, where it would otherwise be
/// the Administrators group: clients check that the pipe belongs to them.
fn sddl(sid: &str) -> String {
    format!("O:{sid}D:P(A;;GA;;;{sid})")
}

/// The security descriptor of the pipe, freed with `LocalFree` on drop.
pub struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from
        // ConvertStringSecurityDescriptorToSecurityDescriptorW, which asks
        // for LocalFree, and is freed only here.
        unsafe { LocalFree(self.0) };
    }
}

impl Descriptor {
    /// The descriptor for the user `sid`.
    pub fn for_user(sid: &str) -> io::Result<Self> {
        let text = wide(&sddl(sid));
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `text` is a NUL-terminated UTF-16 string that outlives
        // the call; `descriptor` receives a LocalAlloc'ed block, owned by
        // the returned value. The size is not needed.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// Creates an instance of the pipe `name` with this descriptor;
    /// `first` only for the very first one. Remote clients are rejected.
    pub fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose
        // descriptor lives as long as `self`; CreateNamedPipeW only reads
        // both during the call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, (&raw mut attributes).cast::<c_void>())
        }
    }
}

/// The SID of the user this process runs as, as text (`S-1-5-21-…`).
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no
    // closing; `token` receives a handle that is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = user_sid(token);
    // SAFETY: `token` is the open handle from OpenProcessToken, closed once.
    unsafe { CloseHandle(token) };
    sid
}

/// The user SID in the access token `token`, as text.
fn user_sid(token: HANDLE) -> io::Result<String> {
    // TOKEN_USER is followed by the SID it points to; ask for the size.
    let mut size = 0u32;
    // SAFETY: a null buffer of length 0 only asks for the size, which is
    // written to `size`; the call fails with ERROR_INSUFFICIENT_BUFFER.
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 elements keep the buffer aligned for TOKEN_USER (pointers).
    let mut buffer = vec![0u64; (size as usize).div_ceil(size_of::<u64>())];
    // SAFETY: the buffer holds at least `size` bytes and is aligned for
    // TOKEN_USER; Windows writes at most `size` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID
    // points into the same buffer, which lives until the end of this
    // function.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is a valid SID (see above); `text` receives a
    // LocalAlloc'ed NUL-terminated string, freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `text` is NUL-terminated (ConvertSidToStringSidW), so the
    // length stops at the NUL and the slice stays inside the string.
    let string = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: `text` came from ConvertSidToStringSidW, which asks for
    // LocalFree, and is not used after this.
    unsafe { LocalFree(text.cast()) };
    Ok(string)
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_sddl_carry_the_sid() {
        let sid = "S-1-5-21-1-2-3-1001";
        assert_eq!(
            pipe_name(sid),
            r"\\.\pipe\lebenszeichen-S-1-5-21-1-2-3-1001-app"
        );
        assert_eq!(
            sddl(sid),
            "O:S-1-5-21-1-2-3-1001D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
        );
    }
}
