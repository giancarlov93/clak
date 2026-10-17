//! The user's registry (HKCU), just enough: read and write strings and bytes, remove
//! values and keys. Every key is given by its path under HKCU, so the tests work on
//! `Software\ClakTests\…` and not on the real ones.

use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_BINARY, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};

pub use windows_sys::Win32::System::Registry::{REG_BINARY as BINARY, REG_EXPAND_SZ as EXPAND_STRING, REG_SZ as STRING};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The name of a value; `None` is the key's default value.
fn value_name(name: Option<&str>) -> Vec<u16> {
    wide(name.unwrap_or(""))
}

fn check(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(code as i32)) }
}

/// An open key, closed when dropped.
pub struct Key(HKEY);

// SAFETY: an HKEY can be used from any thread.
unsafe impl Send for Key {}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key is ours and open.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The key, if it exists.
pub fn open(subkey: &str, write: bool) -> Option<Key> {
    let name = wide(subkey);
    let mut key: HKEY = null_mut();
    let access = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
    // SAFETY: nul-terminated name, valid output pointer.
    let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, access, &mut key) };
    (code == ERROR_SUCCESS).then_some(Key(key))
}

/// The key, created if missing (with the keys above it).
pub fn create(subkey: &str) -> io::Result<Key> {
    let name = wide(subkey);
    let mut key: HKEY = null_mut();
    // SAFETY: nul-terminated name, output pointers valid or null where allowed.
    let code = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            name.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    check(code)?;
    Ok(Key(key))
}

/// Removes the key with everything it contains; if it isn't there, nothing.
pub fn delete_tree(subkey: &str) -> io::Result<()> {
    let name = wide(subkey);
    // SAFETY: nul-terminated name.
    let code = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
    if code != ERROR_SUCCESS && code != ERROR_FILE_NOT_FOUND {
        return check(code);
    }
    // The key itself, if RegDeleteTree removed only the content.
    // SAFETY: as above.
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, name.as_ptr()) };
    Ok(())
}

/// Removes the key only if it is empty (no subkeys).
pub fn delete_if_empty(subkey: &str) {
    let name = wide(subkey);
    // SAFETY: nul-terminated name; if it has subkeys the call just fails.
    unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, name.as_ptr()) };
}

impl Key {
    /// Type and bytes of a value.
    pub fn raw(&self, name: Option<&str>) -> Option<(u32, Vec<u8>)> {
        let name = value_name(name);
        let mut kind = 0u32;
        let mut size = 0u32;
        // SAFETY: first the size, then the data in a buffer of that size.
        let code = unsafe { RegQueryValueExW(self.0, name.as_ptr(), null(), &mut kind, null_mut(), &mut size) };
        if code != ERROR_SUCCESS && code != ERROR_MORE_DATA {
            return None;
        }
        loop {
            let mut data = vec![0u8; size as usize + 2];
            let mut length = data.len() as u32;
            // SAFETY: the buffer is as long as declared.
            let code =
                unsafe { RegQueryValueExW(self.0, name.as_ptr(), null(), &mut kind, data.as_mut_ptr(), &mut length) };
            if code == ERROR_MORE_DATA {
                size = length;
                continue;
            }
            if code != ERROR_SUCCESS {
                return None;
            }
            data.truncate(length as usize);
            return Some((kind, data));
        }
    }

    /// The type of a value, if it exists.
    pub fn kind(&self, name: Option<&str>) -> Option<u32> {
        self.raw(name).map(|(kind, _)| kind)
    }

    /// A string as written: variables (`%USERPROFILE%`) stay variables.
    pub fn string(&self, name: Option<&str>) -> Option<String> {
        let (kind, data) = self.raw(name)?;
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            return None;
        }
        let units: Vec<u16> = data.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }

    /// A string, with variables expanded if the value is of the "expandable" type.
    pub fn string_expanded(&self, name: Option<&str>) -> Option<String> {
        let text = self.string(name)?;
        if self.kind(name) == Some(REG_EXPAND_SZ) { Some(expand(&text)) } else { Some(text) }
    }

    /// The bytes of a binary value.
    pub fn binary(&self, name: Option<&str>) -> Option<Vec<u8>> {
        match self.raw(name)? {
            (REG_BINARY, data) => Some(data),
            _ => None,
        }
    }

    /// Writes a string, of the given type (`STRING` or `EXPAND_STRING`).
    pub fn set_string(&self, name: Option<&str>, value: &str, kind: u32) -> io::Result<()> {
        let name = value_name(name);
        let data = wide(value);
        // SAFETY: nul-terminated UTF-16 data, size in bytes.
        let code =
            unsafe { RegSetValueExW(self.0, name.as_ptr(), 0, kind, data.as_ptr().cast(), (data.len() * 2) as u32) };
        check(code)
    }

    pub fn set_binary(&self, name: Option<&str>, value: &[u8]) -> io::Result<()> {
        let name = value_name(name);
        // SAFETY: data and size match.
        let code = unsafe { RegSetValueExW(self.0, name.as_ptr(), 0, REG_BINARY, value.as_ptr(), value.len() as u32) };
        check(code)
    }

    /// Removes a value; if it isn't there, nothing.
    pub fn delete_value(&self, name: Option<&str>) -> io::Result<()> {
        let name = value_name(name);
        // SAFETY: nul-terminated name.
        let code = unsafe { RegDeleteValueW(self.0, name.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(code) }
    }

    /// The value names (the default one, if it has data, is "").
    pub fn value_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for index in 0.. {
            let mut buffer = vec![0u16; 16384];
            let mut length = buffer.len() as u32;
            // SAFETY: buffer as long as declared; the data isn't needed.
            let code = unsafe {
                RegEnumValueW(self.0, index, buffer.as_mut_ptr(), &mut length, null(), null_mut(), null_mut(), null_mut())
            };
            if code == ERROR_NO_MORE_ITEMS || code != ERROR_SUCCESS {
                break;
            }
            names.push(String::from_utf16_lossy(&buffer[..length as usize]));
        }
        names
    }
}

/// `%USERPROFILE%\bin` → `C:\Users\…\bin`, with this process's environment variables.
pub fn expand(text: &str) -> String {
    let source = wide(text);
    let mut size = 0u32;
    loop {
        let mut buffer = vec![0u16; size.max(1) as usize];
        // SAFETY: buffer as long as declared.
        let needed = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) };
        if needed == 0 {
            return text.to_owned();
        }
        if needed as usize <= buffer.len() {
            let end = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
            return String::from_utf16_lossy(&buffer[..end]);
        }
        size = needed;
    }
}
