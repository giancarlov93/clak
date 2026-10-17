//! Windows: processes, signatures, the TCP table, the pipe, DPAPI.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows_sys::Wdk::System::Threading::NtQueryInformationProcess;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, LocalFree, TRUST_E_NOSIGNATURE,
    TRUST_E_PROVIDER_UNKNOWN, TRUST_E_SUBJECT_FORM_UNKNOWN, UNICODE_STRING,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, TCP_TABLE_OWNER_PID_CONNECTIONS};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::Security::Cryptography::{
    CERT_FIND_SUBJECT_CERT, CERT_INFO, CERT_NAME_STR_REVERSE_FLAG, CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
    CERT_QUERY_FORMAT_FLAG_BINARY, CERT_QUERY_OBJECT_FILE, CERT_X500_NAME_STR, CMSG_SIGNER_INFO, CMSG_SIGNER_INFO_PARAM,
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CertCloseStore, CertFindCertificateInStore, CertFreeCertificateContext,
    CertNameToStrW, CryptMsgClose, CryptMsgGetParam, CryptProtectData, CryptQueryObject, CryptUnprotectData, HCERTSTORE,
    PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
};
use windows_sys::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

use crate::provenance::{ProcessEntry, ProcessLookup};
use crate::secrets::SecretError;

/// A handle that closes itself.
struct Handle(HANDLE);

impl Handle {
    fn process(pid: u32) -> Option<Handle> {
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!handle.is_null()).then_some(Handle(handle))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

// --- processes -----------------------------------------------------------------------

/// The start time of a process, as a FILETIME, or None if it doesn't exist (or can't be read).
pub fn start_time(pid: u32) -> Option<i64> {
    let process = Handle::process(pid)?;
    let mut created = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut other = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 3];
    let [exit, kernel, user] = &mut other;
    let ok = unsafe { GetProcessTimes(process.0, &mut created, exit, kernel, user) };
    (ok != 0).then_some(((created.dwHighDateTime as i64) << 32) | created.dwLowDateTime as i64)
}

/// The full path of a process's executable.
pub fn exe_of(pid: u32) -> Option<String> {
    let process = Handle::process(pid)?;
    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size) };
    (ok != 0).then(|| String::from_utf16_lossy(&buffer[..size as usize]))
}

/// The command line of another process. Only used to give it a name: a label, not proof.
pub fn command_line(pid: u32) -> Option<String> {
    const PROCESS_COMMAND_LINE_INFORMATION: i32 = 60;
    let process = Handle::process(pid)?;
    let mut size = 4096u32;
    for _ in 0..3 {
        // u64 for the alignment of the leading UNICODE_STRING.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let mut needed = 0u32;
        let status = unsafe {
            NtQueryInformationProcess(
                process.0,
                PROCESS_COMMAND_LINE_INFORMATION,
                buffer.as_mut_ptr().cast(),
                size,
                &mut needed,
            )
        };
        if status == 0 {
            let text = unsafe { &*(buffer.as_ptr() as *const UNICODE_STRING) };
            if text.Buffer.is_null() {
                return None;
            }
            let chars = unsafe { std::slice::from_raw_parts(text.Buffer, text.Length as usize / 2) };
            return Some(String::from_utf16_lossy(chars));
        }
        if needed <= size {
            return None;
        }
        size = needed;
    }
    None
}

/// pid → (parent pid, executable name), in one go.
fn process_table() -> HashMap<u32, ProcessEntry> {
    let mut table = HashMap::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
        return table;
    }
    let snapshot = Handle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        table.insert(entry.th32ProcessID, ProcessEntry { parent: entry.th32ParentProcessID, name });
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    table
}

/// The Windows processes: the table is snapshotted once (Toolhelp), the rest is asked
/// of the process when needed.
pub struct Processes {
    table: HashMap<u32, ProcessEntry>,
}

impl Processes {
    pub fn snapshot() -> Self {
        Self { table: process_table() }
    }
}

impl ProcessLookup for Processes {
    fn entry(&self, pid: u32) -> Option<ProcessEntry> {
        self.table.get(&pid).cloned()
    }

    fn start_time(&self, pid: u32) -> Option<i64> {
        start_time(pid)
    }

    fn exe(&self, pid: u32) -> Option<String> {
        exe_of(pid)
    }

    fn command_line(&self, pid: u32) -> Option<String> {
        command_line(pid)
    }
}

// --- channels ------------------------------------------------------------------------

/// The process at the other end of the pipe, asked of the operating system.
pub fn pipe_client_pid(pipe: HANDLE) -> Option<u32> {
    let mut pid = 0u32;
    let ok = unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) };
    (ok != 0 && pid != 0).then_some(pid)
}

/// The process that owns the TCP (IPv4) end with these ports. The same connection
/// shows up twice in the table, once per end: the program's is the row where *its*
/// port is the local one.
pub fn tcp_owner(local_port: u16, remote_port: u16) -> Option<u32> {
    let mut size = 0u32;
    unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_CONNECTIONS, 0) };
    // Between asking "how big" and reading, the table may grow.
    for _ in 0..4 {
        size += 4096;
        let mut buffer = vec![0u32; (size as usize).div_ceil(4)];
        let status = unsafe {
            GetExtendedTcpTable(buffer.as_mut_ptr().cast(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_CONNECTIONS, 0)
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != 0 {
            return None;
        }
        // dwNumEntries, then rows of six DWORDs: state, local_addr, local_port, remote_addr, remote_port, pid.
        let count = buffer[0] as usize;
        let port = |field: u32| u16::from_be((field & 0xFFFF) as u16);
        return buffer[1..]
            .as_chunks::<6>()
            .0
            .iter()
            .take(count)
            .find(|row| port(row[2]) == local_port && port(row[4]) == remote_port)
            .map(|row| row[5]);
    }
    None
}

// --- signature -------------------------------------------------------------------------

type Signature = (String, Option<String>);
type SignatureKey = (String, u128, u64);

fn signatures() -> &'static Mutex<HashMap<SignatureKey, Signature>> {
    static SIGNATURES: OnceLock<Mutex<HashMap<SignatureKey, Signature>>> = OnceLock::new();
    SIGNATURES.get_or_init(Default::default)
}

fn unknown() -> Signature {
    ("unknown".into(), None)
}

/// Who signed an executable: ("valid" | "invalid" | "unsigned" | "unknown", signer).
/// Cached by path, time and size.
pub fn signature(exe: &str) -> Signature {
    let Ok(metadata) = std::fs::metadata(exe) else { return unknown() };
    if !metadata.is_file() {
        return unknown();
    }
    let modified =
        metadata.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let key = (exe.to_string(), modified, metadata.len());
    if let Some(known) = signatures().lock().ok().and_then(|s| s.get(&key).cloned()) {
        return known;
    }
    let read = read_signature(exe);
    // An "I don't know" may be a PowerShell that was late: it isn't cached.
    if read.0 != "unknown"
        && let Ok(mut known) = signatures().lock() {
            known.insert(key, read.clone());
        }
    read
}

/// In-house first: the signature embedded in the file is verified with WinVerifyTrust
/// in a few milliseconds. But a Windows executable (notepad.exe) is only
/// catalog-signed, and so shows up as "unsigned": for those, and only for those,
/// PowerShell is asked, since it knows how to search the catalogs.
fn read_signature(exe: &str) -> Signature {
    match verify_embedded(exe) {
        0 => ("valid".into(), embedded_signer(exe)),
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => ask_powershell(exe),
        _ => ("invalid".into(), embedded_signer(exe)),
    }
}

fn verify_embedded(exe: &str) -> i32 {
    let path = wide(exe);
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
    data.dwUIChoice = WTD_UI_NONE;
    data.fdwRevocationChecks = WTD_REVOKE_NONE;
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous.pFile = &mut file;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    // We ask who signed, not whether the certificate was revoked this morning: no network.
    data.dwProvFlags = WTD_REVOCATION_CHECK_NONE;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let verdict = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&mut data as *mut WINTRUST_DATA).cast()) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&mut data as *mut WINTRUST_DATA).cast()) };
    verdict
}

/// The name of whoever signed the file: the CN of the signer's certificate, read from
/// the subject written with the CN first (`CN=…, O=…, C=US`).
fn embedded_signer(exe: &str) -> Option<String> {
    let path = wide(exe);
    let mut encoding = 0u32;
    let mut content = 0u32;
    let mut format = 0u32;
    let mut store: HCERTSTORE = std::ptr::null_mut();
    let mut message: *mut c_void = std::ptr::null_mut();
    let ok = unsafe {
        CryptQueryObject(
            CERT_QUERY_OBJECT_FILE,
            path.as_ptr().cast(),
            CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED,
            CERT_QUERY_FORMAT_FLAG_BINARY,
            0,
            &mut encoding,
            &mut content,
            &mut format,
            &mut store,
            &mut message,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let subject = (|| {
        let mut size = 0u32;
        if unsafe { CryptMsgGetParam(message, CMSG_SIGNER_INFO_PARAM, 0, std::ptr::null_mut(), &mut size) } == 0 {
            return None;
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        if unsafe { CryptMsgGetParam(message, CMSG_SIGNER_INFO_PARAM, 0, buffer.as_mut_ptr().cast(), &mut size) } == 0 {
            return None;
        }
        let signer = unsafe { &*(buffer.as_ptr() as *const CMSG_SIGNER_INFO) };
        let mut wanted: CERT_INFO = unsafe { std::mem::zeroed() };
        wanted.Issuer = signer.Issuer;
        wanted.SerialNumber = signer.SerialNumber;
        let certificate = unsafe {
            CertFindCertificateInStore(
                store,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                CERT_FIND_SUBJECT_CERT,
                (&wanted as *const CERT_INFO).cast(),
                std::ptr::null(),
            )
        };
        if certificate.is_null() {
            return None;
        }
        let name = unsafe { &(*(*certificate).pCertInfo).Subject } as *const CRYPT_INTEGER_BLOB;
        let kind = CERT_X500_NAME_STR | CERT_NAME_STR_REVERSE_FLAG;
        let length = unsafe { CertNameToStrW(X509_ASN_ENCODING, name, kind, std::ptr::null_mut(), 0) };
        let mut text = vec![0u16; length.max(1) as usize];
        let written = unsafe { CertNameToStrW(X509_ASN_ENCODING, name, kind, text.as_mut_ptr(), length) };
        unsafe { CertFreeCertificateContext(certificate) };
        (written > 1).then(|| String::from_utf16_lossy(&text[..written as usize - 1]))
    })();
    unsafe {
        CryptMsgClose(message);
        CertCloseStore(store, 0);
    }
    crate::provenance::common_name_of(&subject?)
}

fn ask_powershell(exe: &str) -> Signature {
    // The path goes through the environment, not the command line: no quoting.
    const SCRIPT: &str = "$s = Get-AuthenticodeSignature -LiteralPath $env:CLAK_EXE; \
        [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
        $s.Status; if ($s.SignerCertificate) { $s.SignerCertificate.Subject }";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // The full path: a "powershell.exe" left in the working directory must not be able to answer in its place.
    let shell = system_directory()
        .map(|dir| dir.join("WindowsPowerShell").join("v1.0").join("powershell.exe"))
        .filter(|path| path.is_file())
        .map(|path| path.into_os_string())
        .unwrap_or_else(|| "powershell".into());
    let child = std::process::Command::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("CLAK_EXE", exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let Ok(mut child) = child else { return unknown() };
    let Some(mut stdout) = child.stdout.take() else { return unknown() };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut output);
        let _ = sender.send(output);
    });
    let output = match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(output) => output,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return unknown();
        }
    };
    let _ = child.wait();
    let text = String::from_utf8_lossy(&output);
    let lines: Vec<&str> = text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    match lines.first() {
        None => unknown(),
        Some(&"NotSigned") => ("unsigned".into(), None),
        Some(status) => (
            if *status == "Valid" { "valid" } else { "invalid" }.into(),
            lines.get(1).and_then(|subject| crate::provenance::common_name_of(subject)),
        ),
    }
}

/// The system directory (`C:\Windows\System32`), asked of the OS and not the environment.
fn system_directory() -> Option<std::path::PathBuf> {
    let mut buffer = vec![0u16; 1024];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length]).into())
}

// --- DPAPI -----------------------------------------------------------------------------

/// Clak's entropy: a blob encrypted with a different one doesn't decrypt.
const ENTROPY: &[u8] = b"clak/connectors";

/// CryptProtectData / CryptUnprotectData with user scope, like
/// `ProtectedData` with `DataProtectionScope.CurrentUser`.
pub fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>, SecretError> {
    let data = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_ptr() as *mut u8 };
    let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &data,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &data,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        return Err(SecretError(if protect {
            format!("Windows didn't encrypt the token: {error}")
        } else {
            format!("Windows didn't decrypt the token: {error}")
        }));
    }
    let result = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_firma_si_legge_sia_dal_file_sia_dal_catalogo_di_windows() {
        let system = system_directory().unwrap();
        // Catalog-only signature: there's nothing in the file, PowerShell knows.
        let notepad = system.join("notepad.exe");
        if notepad.is_file() {
            assert_eq!(signature(notepad.to_str().unwrap()), ("valid".into(), Some("Microsoft Windows".into())));
        }
        // No signature: this very test executable.
        let me = std::env::current_exe().unwrap();
        assert_eq!(signature(me.to_str().unwrap()), ("unsigned".into(), None));
        let missing = std::env::temp_dir().join(format!("non-esiste-{}.exe", std::process::id()));
        assert_eq!(signature(missing.to_str().unwrap()), unknown());
    }

    #[test]
    fn la_firma_di_dotnet_e_di_dotnet() {
        // A .NET runtime DLL, if present: the Microsoft signature says ".NET".
        let Ok(versions) = std::fs::read_dir(r"C:\Program Files\dotnet\shared\Microsoft.NETCore.App") else { return };
        let Some(corelib) = versions
            .filter_map(Result::ok)
            .map(|v| v.path().join("System.Private.CoreLib.dll"))
            .find(|p| p.is_file())
        else {
            return;
        };
        assert_eq!(signature(corelib.to_str().unwrap()), ("valid".into(), Some(".NET".into())));
    }

    #[test]
    fn la_firma_incorporata_si_verifica_in_casa() {
        // An executable with the signature inside the file: the first one found among the usual ones.
        let candidates = [
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Program Files\Git\cmd\git.exe",
            r"C:\Program Files\Microsoft VS Code\Code.exe",
            r"C:\Program Files\dotnet\dotnet.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ];
        let Some(exe) = candidates.iter().find(|p| std::path::Path::new(p).is_file()) else { return };
        assert_eq!(verify_embedded(exe), 0, "{exe}");
        let (signed, signer) = signature(exe);
        assert_eq!(signed, "valid");
        assert!(signer.is_some_and(|s| !s.is_empty()), "{exe}");
    }

    #[test]
    fn la_tabella_tcp_trova_chi_tiene_una_connessione() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hub = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect(("127.0.0.1", hub)).unwrap();
        let program = client.local_addr().unwrap().port();
        let _accepted = listener.accept().unwrap();
        assert_eq!(tcp_owner(program, hub), Some(std::process::id()));
        assert_eq!(tcp_owner(program, hub.wrapping_add(1).max(1)), None);
    }

    /// Runs a script in Windows PowerShell (i.e. .NET Framework) and returns its output.
    fn powershell(script: &str, env: &[(&str, &str)]) -> String {
        let shell = system_directory().unwrap().join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        let output = std::process::Command::new(shell)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .envs(env.iter().copied())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn i_blob_dpapi_sono_quelli_di_protecteddata() {
        use base64::Engine as _;
        let base64 = base64::engine::general_purpose::STANDARD;
        // Encrypted by ProtectedData with the same entropy: it decrypts here.
        let sealed = powershell(
            "Add-Type -AssemblyName System.Security; \
             $e = [Text.Encoding]::UTF8.GetBytes('clak/connectors'); \
             $b = [Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes('token-da-dotnet'), $e, 'CurrentUser'); \
             [Convert]::ToBase64String($b)",
            &[],
        );
        let plain = dpapi(&base64.decode(&sealed).unwrap(), false).unwrap();
        assert_eq!(plain, b"token-da-dotnet");
        // And vice versa: a blob encrypted here is decrypted by ProtectedData.
        let sealed = base64.encode(dpapi(b"token-da-rust", true).unwrap());
        let plain = powershell(
            "Add-Type -AssemblyName System.Security; \
             $e = [Text.Encoding]::UTF8.GetBytes('clak/connectors'); \
             $b = [Security.Cryptography.ProtectedData]::Unprotect([Convert]::FromBase64String($env:CLAK_BLOB), $e, 'CurrentUser'); \
             [Text.Encoding]::UTF8.GetString($b)",
            &[("CLAK_BLOB", &sealed)],
        );
        assert_eq!(plain, "token-da-rust");
    }

    #[test]
    fn la_riga_di_comando_e_il_percorso_di_questo_processo() {
        let me = std::process::id();
        let exe = exe_of(me).unwrap();
        // Only the name: inside an MSIX package the path seen from here is the virtualized one.
        let current = std::env::current_exe().unwrap();
        assert!(exe.to_lowercase().ends_with(&current.file_name().unwrap().to_str().unwrap().to_lowercase()), "{exe}");
        assert!(!command_line(me).unwrap().is_empty());
        assert!(start_time(me).unwrap() > 0);
        assert!(process_table().contains_key(&me));
        assert_eq!(start_time(0), None);
    }
}
