//! macOS: processes (libproc), TCP ports, the Unix socket, code signatures, Keychain.
//!
//! Everything works on the same user's processes without special permissions, but only
//! outside the sandbox: the app can't be sandboxed.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::os::fd::RawFd;
use std::sync::{Mutex, OnceLock};

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::array::{
    CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef,
};
use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef, OSStatus};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataGetTypeID, CFDataRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{CFNumberGetTypeID, CFNumberRef};
use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};
use core_foundation_sys::url::CFURLRef;

use crate::provenance::{ProcessEntry, ProcessLookup, Signature};
use crate::secrets::SecretError;

// --- processes -----------------------------------------------------------------------

/// Parent, start time (microseconds since the epoch) and short name of a process.
pub struct BsdInfo {
    pub parent: u32,
    pub started: i64,
    pub comm: String,
}

pub fn bsd_info(pid: u32) -> Option<BsdInfo> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    let comm = unsafe { CStr::from_ptr(info.pbi_comm.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Some(BsdInfo {
        parent: info.pbi_ppid,
        started: info.pbi_start_tvsec as i64 * 1_000_000 + info.pbi_start_tvusec as i64,
        comm,
    })
}

/// The full path of a process's executable.
pub fn path_of(pid: u32) -> Option<String> {
    if pid > i32::MAX as u32 {
        return None;
    }
    let mut buffer = vec![0u8; 4 * libc::PATH_MAX as usize];
    let length =
        unsafe { libc::proc_pidpath(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buffer[..length as usize]).into_owned())
}

/// The command line of another process. Only used to give it a name: a label, not proof.
pub fn command_line(pid: u32) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
    {
        return None;
    }
    let mut buffer = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size <= 4
    {
        return None;
    }
    let argc = i32::from_ne_bytes(buffer[..4].try_into().ok()?).max(0) as usize;
    // After argc: the executable path, some zeros, then the arguments separated by zeros.
    let mut at = 4;
    while at < size && buffer[at] != 0 {
        at += 1;
    }
    while at < size && buffer[at] == 0 {
        at += 1;
    }
    let mut arguments = Vec::new();
    while at < size && arguments.len() < argc {
        let start = at;
        while at < size && buffer[at] != 0 {
            at += 1;
        }
        arguments.push(String::from_utf8_lossy(&buffer[start..at]).into_owned());
        at += 1;
    }
    Some(arguments.join(" "))
}

/// The Mac's processes, asked of libproc one by one.
pub struct Processes;

impl ProcessLookup for Processes {
    fn entry(&self, pid: u32) -> Option<ProcessEntry> {
        bsd_info(pid).map(|info| ProcessEntry {
            parent: info.parent,
            name: info.comm,
        })
    }

    fn start_time(&self, pid: u32) -> Option<i64> {
        bsd_info(pid).map(|info| info.started)
    }

    fn exe(&self, pid: u32) -> Option<String> {
        path_of(pid)
    }

    fn command_line(&self, pid: u32) -> Option<String> {
        command_line(pid)
    }
}

// --- channels ------------------------------------------------------------------------

/// The process at the other end of a Unix socket, asked of the kernel (`LOCAL_PEERPID`).
pub fn unix_peer_pid(fd: RawFd) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut size = size_of::<libc::pid_t>() as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut size,
        )
    };
    (ok == 0 && pid > 0).then_some(pid as u32)
}

// `socket_fdinfo` from <sys/proc_info.h>, which libc doesn't declare: only three fields
// are read, at their offsets. proc_fileinfo takes 24 bytes; in socket_info, after
// vinfo_stat (136) and the fields up to the two sockbuf_info, soi_kind is at 232 and the
// soi_proto union at 240, where tcp_sockinfo starts with in_sockinfo: insi_fport, insi_lport.
// The whole struct is 792 bytes, and that's what the kernel must return.
const PROC_PIDFDSOCKETINFO: i32 = 3;
const SOCKET_FDINFO_SIZE: usize = 792;
const SOI_KIND: usize = 24 + 232;
const INSI_FPORT: usize = 24 + 240;
const INSI_LPORT: usize = INSI_FPORT + 4;
const SOCKINFO_TCP: i32 = 2;

/// The process that owns the TCP end with these ports. macOS has no connection table
/// with the pid: the processes and their descriptors are walked, as `lsof` does. The
/// same connection shows up twice, once per end: the program's is the one where *its*
/// port is the local one.
pub fn tcp_owner(local_port: u16, remote_port: u16) -> Option<u32> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return None;
    }
    let mut pids = vec![0i32; count as usize + 64];
    let found = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * size_of::<i32>()) as i32,
        )
    };
    if found <= 0 {
        return None;
    }
    pids.iter()
        .take(found as usize)
        .copied()
        .filter(|&pid| pid > 0)
        .find(|&pid| owns(pid, local_port, remote_port))
        .map(|pid| pid as u32)
}

fn owns(pid: i32, local_port: u16, remote_port: u16) -> bool {
    let needed =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return false;
    }
    let stride = size_of::<libc::proc_fdinfo>();
    let mut fds: Vec<libc::proc_fdinfo> =
        vec![unsafe { std::mem::zeroed() }; needed as usize / stride + 16];
    let size = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            (fds.len() * stride) as i32,
        )
    };
    if size <= 0 {
        return false;
    }
    let mut info = [0u64; SOCKET_FDINFO_SIZE / 8];
    for fd in fds.iter().take(size as usize / stride) {
        if fd.proc_fdtype != libc::PROX_FDTYPE_SOCKET as u32 {
            continue;
        }
        let read = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDSOCKETINFO,
                info.as_mut_ptr().cast(),
                SOCKET_FDINFO_SIZE as i32,
            )
        };
        if read != SOCKET_FDINFO_SIZE as i32 {
            continue;
        }
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(info.as_ptr().cast(), SOCKET_FDINFO_SIZE) };
        let int = |at: usize| i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
        if int(SOI_KIND) != SOCKINFO_TCP {
            continue;
        }
        let port = |field: i32| u16::from_be(field as u16);
        if port(int(INSI_LPORT)) == local_port && port(int(INSI_FPORT)) == remote_port {
            return true;
        }
    }
    false
}

// --- Security.framework ----------------------------------------------------------------

type SecCodeRef = *mut c_void;
type SecStaticCodeRef = *mut c_void;
type SecRequirementRef = *mut c_void;
type SecCertificateRef = *mut c_void;

const ERR_SEC_SUCCESS: OSStatus = 0;
const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
const ERR_SEC_CS_UNSIGNED: OSStatus = -67062;
const K_SEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;
const K_SEC_CS_DO_NOT_VALIDATE_RESOURCES: u32 = 1 << 2;
const K_SEC_CODE_SIGNATURE_ADHOC: u32 = 0x0002;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopyGuestWithAttributes(
        host: SecCodeRef,
        attributes: CFDictionaryRef,
        flags: u32,
        guest: *mut SecCodeRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(
        code: SecCodeRef,
        flags: u32,
        requirement: SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopyStaticCode(
        code: SecCodeRef,
        flags: u32,
        static_code: *mut SecStaticCodeRef,
    ) -> OSStatus;
    fn SecStaticCodeCreateWithPath(
        path: CFURLRef,
        flags: u32,
        static_code: *mut SecStaticCodeRef,
    ) -> OSStatus;
    fn SecStaticCodeCheckValidity(
        code: SecStaticCodeRef,
        flags: u32,
        requirement: SecRequirementRef,
    ) -> OSStatus;
    fn SecCodeCopySigningInformation(
        code: SecStaticCodeRef,
        flags: u32,
        information: *mut CFDictionaryRef,
    ) -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: u32,
        requirement: *mut SecRequirementRef,
    ) -> OSStatus;
    fn SecCertificateCopySubjectSummary(certificate: SecCertificateRef) -> CFStringRef;
    fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemDelete(query: CFDictionaryRef) -> OSStatus;
    fn SecCopyErrorMessageString(status: OSStatus, reserved: *mut c_void) -> CFStringRef;

    static kSecGuestAttributePid: CFStringRef;
    static kSecCodeInfoFlags: CFStringRef;
    static kSecCodeInfoCertificates: CFStringRef;
    static kSecCodeInfoTeamIdentifier: CFStringRef;

    static kSecClass: CFStringRef;
    static kSecClassGenericPassword: CFStringRef;
    static kSecAttrService: CFStringRef;
    static kSecAttrAccount: CFStringRef;
    static kSecAttrLabel: CFStringRef;
    static kSecAttrDescription: CFStringRef;
    static kSecValueData: CFStringRef;
    static kSecReturnData: CFStringRef;
    static kSecReturnAttributes: CFStringRef;
    static kSecMatchLimit: CFStringRef;
    static kSecMatchLimitOne: CFStringRef;
}

/// A Core Foundation object that releases itself.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn key(constant: CFStringRef) -> CFType {
    unsafe { CFString::wrap_under_get_rule(constant) }.as_CFType()
}

fn dictionary(pairs: &[(CFType, CFType)]) -> CFDictionary<CFType, CFType> {
    CFDictionary::from_CFType_pairs(pairs)
}

fn string_of(value: CFTypeRef) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    Some(unsafe { CFString::wrap_under_get_rule(value as CFStringRef) }.to_string())
}

/// A compiled requirement never changes again: it can be read from several threads.
struct Requirement(SecRequirementRef);
unsafe impl Send for Requirement {}
unsafe impl Sync for Requirement {}

fn apple_rooted() -> SecRequirementRef {
    static REQUIREMENT: OnceLock<Requirement> = OnceLock::new();
    REQUIREMENT
        .get_or_init(|| {
            let text = CFString::new("anchor apple generic");
            let mut requirement: SecRequirementRef = std::ptr::null_mut();
            let status = unsafe {
                SecRequirementCreateWithString(text.as_concrete_TypeRef(), 0, &mut requirement)
            };
            Requirement(if status == ERR_SEC_SUCCESS {
                requirement
            } else {
                std::ptr::null_mut()
            })
        })
        .0
}

/// Who signed an executable.
///
/// On Apple Silicon every executable is at least signed *ad hoc* (the linker does it),
/// and an ad hoc signature doesn't say who made it: it counts as "unsigned". A
/// signature that doesn't chain up to Apple's authorities (a homemade certificate with
/// any name) is "invalid": otherwise anyone could call themselves Anthropic.
///
/// For a live process what is running gets verified (the kernel already checks the
/// loaded pages), and it's fast; without a pid the file gets verified, and cached.
pub fn signature(exe: &str, pid: Option<u32>) -> Signature {
    if let Some(live) = pid.and_then(running) {
        return live;
    }
    cached_signature(exe)
}

fn running(pid: u32) -> Option<Signature> {
    let attributes = dictionary(&[(
        key(unsafe { kSecGuestAttributePid }),
        CFNumber::from(pid as i32).as_CFType(),
    )]);
    let mut code: SecCodeRef = std::ptr::null_mut();
    let status = unsafe {
        SecCodeCopyGuestWithAttributes(
            std::ptr::null_mut(),
            attributes.as_concrete_TypeRef(),
            0,
            &mut code,
        )
    };
    if status != ERR_SEC_SUCCESS || code.is_null() {
        return None;
    }
    let code = Owned(code);
    let validity = unsafe { SecCodeCheckValidity(code.0 as SecCodeRef, 0, std::ptr::null_mut()) };
    let mut static_code: SecStaticCodeRef = std::ptr::null_mut();
    if unsafe { SecCodeCopyStaticCode(code.0 as SecCodeRef, 0, &mut static_code) }
        != ERR_SEC_SUCCESS
        || static_code.is_null()
    {
        return None;
    }
    let static_code = Owned(static_code);
    Some(judge(
        validity,
        static_code.0 as SecStaticCodeRef,
        || unsafe { SecCodeCheckValidity(code.0 as SecCodeRef, 0, apple_rooted()) },
    ))
}

/// The file's signature, without a process.
pub fn file_signature(exe: &str) -> Signature {
    if !std::path::Path::new(exe).exists() {
        return Signature::unknown();
    }
    let Some(url) = CFURL::from_path(exe, false) else {
        return Signature::unknown();
    };
    let mut code: SecStaticCodeRef = std::ptr::null_mut();
    if unsafe { SecStaticCodeCreateWithPath(url.as_concrete_TypeRef(), 0, &mut code) }
        != ERR_SEC_SUCCESS
        || code.is_null()
    {
        return Signature::unknown();
    }
    let code = Owned(code);
    // A large app's resources would all be read: the executable is enough.
    let flags = K_SEC_CS_DO_NOT_VALIDATE_RESOURCES;
    let status = unsafe {
        SecStaticCodeCheckValidity(code.0 as SecStaticCodeRef, flags, std::ptr::null_mut())
    };
    judge(status, code.0 as SecStaticCodeRef, || unsafe {
        SecStaticCodeCheckValidity(code.0 as SecStaticCodeRef, flags, apple_rooted())
    })
}

fn judge(status: OSStatus, code: SecStaticCodeRef, rooted: impl Fn() -> OSStatus) -> Signature {
    if status == ERR_SEC_CS_UNSIGNED {
        return Signature::unsigned();
    }
    let mut raw: CFDictionaryRef = std::ptr::null();
    if unsafe { SecCodeCopySigningInformation(code, K_SEC_CS_SIGNING_INFORMATION, &mut raw) }
        != ERR_SEC_SUCCESS
        || raw.is_null()
    {
        let signed = if status == ERR_SEC_SUCCESS {
            "unknown"
        } else {
            "invalid"
        };
        return Signature {
            signed: signed.into(),
            signer: None,
            team: None,
        };
    }
    let info = Owned(raw as CFTypeRef);
    let get = |name: CFStringRef| unsafe {
        CFDictionaryGetValue(info.0 as CFDictionaryRef, name as *const c_void)
    };

    let flags = {
        let value = get(unsafe { kSecCodeInfoFlags });
        if !value.is_null() && unsafe { CFGetTypeID(value) == CFNumberGetTypeID() } {
            unsafe { CFNumber::wrap_under_get_rule(value as CFNumberRef) }
                .to_i64()
                .unwrap_or(0) as u32
        } else {
            0
        }
    };
    let certificates = get(unsafe { kSecCodeInfoCertificates });
    let first_certificate = if !certificates.is_null()
        && unsafe { CFGetTypeID(certificates) == CFArrayGetTypeID() }
        && unsafe { CFArrayGetCount(certificates as CFArrayRef) } > 0
    {
        unsafe { CFArrayGetValueAtIndex(certificates as CFArrayRef, 0) }
    } else {
        std::ptr::null()
    };
    if flags & K_SEC_CODE_SIGNATURE_ADHOC != 0 || first_certificate.is_null() {
        return Signature::unsigned();
    }
    let team = string_of(get(unsafe { kSecCodeInfoTeamIdentifier }));
    let summary =
        unsafe { SecCertificateCopySubjectSummary(first_certificate as SecCertificateRef) };
    let summary = (!summary.is_null())
        .then(|| unsafe { CFString::wrap_under_create_rule(summary) }.to_string());
    let signer = crate::provenance::signer_name_macos(summary.as_deref(), team.as_deref());
    if status != ERR_SEC_SUCCESS || rooted() != ERR_SEC_SUCCESS {
        return Signature {
            signed: "invalid".into(),
            signer,
            team: None,
        };
    }
    Signature {
        signed: "valid".into(),
        signer,
        team,
    }
}

type SignatureKey = (String, u128, u64);

/// File signatures, cached by path, time and size.
fn cached_signature(exe: &str) -> Signature {
    static KNOWN: OnceLock<Mutex<HashMap<SignatureKey, Signature>>> = OnceLock::new();
    let Ok(metadata) = std::fs::metadata(exe) else {
        return Signature::unknown();
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = (exe.to_string(), modified, metadata.len());
    let known = KNOWN.get_or_init(Default::default);
    if let Some(hit) = known.lock().ok().and_then(|k| k.get(&key).cloned()) {
        return hit;
    }
    let read = file_signature(exe);
    if read.signed != "unknown"
        && let Ok(mut known) = known.lock()
    {
        known.insert(key, read.clone());
    }
    read
}

// --- Keychain --------------------------------------------------------------------------

fn query(service: &str, account: &str) -> Vec<(CFType, CFType)> {
    unsafe {
        vec![
            (key(kSecClass), key(kSecClassGenericPassword)),
            (key(kSecAttrService), CFString::new(service).as_CFType()),
            (key(kSecAttrAccount), CFString::new(account).as_CFType()),
        ]
    }
}

pub fn keychain_read(service: &str, account: &str) -> Option<String> {
    let mut pairs = query(service, account);
    unsafe {
        pairs.push((key(kSecReturnData), CFBoolean::true_value().as_CFType()));
        pairs.push((key(kSecMatchLimit), key(kSecMatchLimitOne)));
    }
    let mut found: CFTypeRef = std::ptr::null();
    if unsafe { SecItemCopyMatching(dictionary(&pairs).as_concrete_TypeRef(), &mut found) }
        != ERR_SEC_SUCCESS
        || found.is_null()
    {
        return None;
    }
    let found = Owned(found);
    if unsafe { CFGetTypeID(found.0) != CFDataGetTypeID() } {
        return None;
    }
    let data = found.0 as CFDataRef;
    let bytes = unsafe {
        std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize)
    };
    String::from_utf8(bytes.to_vec()).ok()
}

/// Whether it's there, without reading it: the Keychain needn't ask the user anything.
pub fn keychain_has(service: &str, account: &str) -> bool {
    let mut pairs = query(service, account);
    unsafe {
        pairs.push((
            key(kSecReturnAttributes),
            CFBoolean::true_value().as_CFType(),
        ));
        pairs.push((key(kSecMatchLimit), key(kSecMatchLimitOne)));
    }
    let mut found: CFTypeRef = std::ptr::null();
    let status =
        unsafe { SecItemCopyMatching(dictionary(&pairs).as_concrete_TypeRef(), &mut found) };
    drop(Owned(found));
    status == ERR_SEC_SUCCESS
}

pub fn keychain_write(
    service: &str,
    account: &str,
    secret: Option<&str>,
    label: &str,
) -> Result<(), SecretError> {
    // Remove and add again: updating an item written by another copy of the app would
    // go through that copy's permissions, a new item belongs to this one.
    let removed =
        unsafe { SecItemDelete(dictionary(&query(service, account)).as_concrete_TypeRef()) };
    if removed != ERR_SEC_SUCCESS && removed != ERR_SEC_ITEM_NOT_FOUND {
        return Err(keychain_error(removed));
    }
    let Some(secret) = secret else { return Ok(()) };
    // This is what the user reads in Keychain Access next to the item.
    let description = if account.starts_with("api_") {
        "Clak REST API secret"
    } else if account.ends_with("#oauth") {
        "Clak connector sign-in (OAuth)"
    } else {
        "Clak connector token"
    };
    let mut item = query(service, account);
    unsafe {
        item.push((
            key(kSecValueData),
            CFData::from_buffer(secret.as_bytes()).as_CFType(),
        ));
        item.push((key(kSecAttrLabel), CFString::new(label).as_CFType()));
        item.push((
            key(kSecAttrDescription),
            CFString::new(description).as_CFType(),
        ));
    }
    let added = unsafe {
        SecItemAdd(
            dictionary(&item).as_concrete_TypeRef(),
            std::ptr::null_mut(),
        )
    };
    if added != ERR_SEC_SUCCESS {
        return Err(keychain_error(added));
    }
    Ok(())
}

fn keychain_error(status: OSStatus) -> SecretError {
    let message = unsafe { SecCopyErrorMessageString(status, std::ptr::null_mut()) };
    let text = if message.is_null() {
        format!("error {status}")
    } else {
        unsafe { CFString::wrap_under_create_rule(message) }.to_string()
    };
    SecretError(format!("The Keychain didn't save the token: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_tells_apple_a_developer_and_the_linker_apart() {
        // A system binary: signed by Apple, without a Team ID.
        let ls = file_signature("/bin/ls");
        assert_eq!(ls.signed, "valid");
        assert_eq!(ls.signer.as_deref(), Some("Apple"));
        // This very test process: signed only by the linker, i.e. unsigned.
        let me = std::process::id();
        assert_eq!(
            signature(&path_of(me).unwrap(), Some(me)).signed,
            "unsigned"
        );
        assert_eq!(file_signature("/tmp/does-not-exist-clak").signed, "unknown");
    }

    #[test]
    fn the_tcp_table_finds_who_holds_a_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hub = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", hub)).unwrap();
        let program = client.local_addr().unwrap().port();
        let _accepted = listener.accept().unwrap();
        assert_eq!(tcp_owner(program, hub), Some(std::process::id()));
    }
}
