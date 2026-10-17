//! The local time, for file names (`20261017-153012-345-…`) and the shim's log lines.
//! The standard library doesn't provide it: it is asked of the system.

/// An instant in the user's time zone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Local {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

impl Local {
    pub fn now() -> Local {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::SYSTEMTIME;
            use windows_sys::Win32::System::SystemInformation::GetLocalTime;
            // SAFETY: SYSTEMTIME is a struct of integers only; the system fills it.
            let mut time: SYSTEMTIME = unsafe { std::mem::zeroed() };
            unsafe { GetLocalTime(&mut time) };
            Local {
                year: time.wYear.into(),
                month: time.wMonth.into(),
                day: time.wDay.into(),
                hour: time.wHour.into(),
                minute: time.wMinute.into(),
                second: time.wSecond.into(),
                millis: time.wMilliseconds.into(),
            }
        }
        #[cfg(unix)]
        {
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let seconds = since.as_secs() as libc::time_t;
            // SAFETY: `tm` holds only integers (and a pointer that localtime_r fills); both
            // pointers are valid for the whole call.
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            unsafe { libc::localtime_r(&seconds, &mut tm) };
            Local {
                year: (tm.tm_year + 1900) as u32,
                month: (tm.tm_mon + 1) as u32,
                day: tm.tm_mday as u32,
                hour: tm.tm_hour as u32,
                minute: tm.tm_min as u32,
                second: tm.tm_sec as u32,
                millis: since.subsec_millis(),
            }
        }
    }

    /// `yyyyMMdd-HHmmss-fff`: sorts by itself, and two files from the same second don't collide.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{:03}",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }

    /// `HH:mm:ss`.
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_timbro_ha_la_forma_di_sempre() {
        let stamp = Local::now().stamp();
        assert_eq!(stamp.len(), "20261017-153012-345".len());
        assert_eq!(&stamp[8..9], "-");
        assert_eq!(Local::now().clock().len(), 8);
    }
}
