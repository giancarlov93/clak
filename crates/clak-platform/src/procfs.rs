//! Linux: the processes and the sockets, read from `/proc`.
//!
//! Everything comes from files, so the folder is a parameter: the real one is `/proc`, the
//! tests' one a folder of their own with the same shape. Only what a user may read of their
//! own processes is used: `stat` (parent, short name, start) can be read for every process,
//! `exe` and `fd` only for one's own, which are the ones that matter (the agents and the
//! programs run as the user; sshd and systemd are recognized by their short name).

use crate::provenance::{ProcessEntry, ProcessLookup};
use std::path::{Path, PathBuf};

/// A `/proc`: the system's, or a copy of its shape.
pub struct Proc {
    root: PathBuf,
}

/// What `stat` says of a process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    pub parent: u32,
    /// The short name (`comm`): the executable's file name, cut to 15 bytes.
    pub comm: String,
    /// When it started, in clock ticks since boot: it never changes while the process lives.
    pub started: i64,
}

/// `stat`'s line: `pid (comm) state ppid … starttime …`. The name can hold spaces and
/// parentheses, so it ends at the last `)`; after it the fields are counted from `state`
/// (field 3): the parent is field 4, the start field 22.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_string();
    let fields: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    Some(Stat {
        parent: fields.get(1)?.parse().ok()?,
        comm,
        started: fields.get(19)?.parse().ok()?,
    })
}

/// The inode of the TCP socket whose local port is `local_port` and remote port
/// `remote_port`, both ends on loopback, in a `/proc/net/tcp` or `tcp6` table.
pub fn socket_inode(table: &str, local_port: u16, remote_port: u16) -> Option<u64> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (local, local_at) = endpoint(fields.get(1)?)?;
        let (remote, remote_at) = endpoint(fields.get(2)?)?;
        let matches = local_at == local_port && remote_at == remote_port && local && remote;
        matches.then(|| fields.get(9)?.parse().ok()).flatten()
    })
}

/// `0100007F:1F90` → (loopback, 8080). The address is written as the kernel keeps it: each
/// 32-bit word in the machine's byte order, so its bytes are in network order once read
/// back in the same one.
fn endpoint(text: &str) -> Option<(bool, u16)> {
    let (address, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::with_capacity(16);
    for chunk in address.as_bytes().chunks(8) {
        let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    let loopback = match bytes.len() {
        4 => std::net::Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?).is_loopback(),
        16 => {
            let v6 = std::net::Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?);
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        _ => return None,
    };
    Some((loopback, port))
}

/// When the system booted, in seconds since the epoch: the `btime` line of `/proc/stat`.
pub fn parse_boot_time(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|value| value.trim().parse().ok())
}

/// A start in clock ticks since boot, as an instant.
pub fn instant(boot: u64, ticks: i64, hertz: u64) -> Option<std::time::SystemTime> {
    if ticks <= 0 || hertz == 0 {
        return None;
    }
    let since_boot = std::time::Duration::from_secs_f64(ticks as f64 / hertz as f64);
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(boot) + since_boot)
}

impl Proc {
    #[cfg(target_os = "linux")]
    pub fn system() -> Proc {
        Proc::at("/proc")
    }

    pub fn at(root: impl Into<PathBuf>) -> Proc {
        Proc { root: root.into() }
    }

    fn of(&self, pid: u32) -> PathBuf {
        self.root.join(pid.to_string())
    }

    pub fn stat(&self, pid: u32) -> Option<Stat> {
        parse_stat(&std::fs::read_to_string(self.of(pid).join("stat")).ok()?)
    }

    /// When the system booted (see [`parse_boot_time`]).
    pub fn boot_time(&self) -> Option<u64> {
        parse_boot_time(&std::fs::read_to_string(self.root.join("stat")).ok()?)
    }

    /// The process holding the loopback connection from `local_port` to `remote_port`:
    /// the socket's inode from the tables, then the process with a descriptor on it.
    /// Only the user's own processes can be looked into.
    pub fn tcp_owner(&self, local_port: u16, remote_port: u16) -> Option<u32> {
        let inode = ["tcp", "tcp6"].iter().find_map(|table| {
            let text = std::fs::read_to_string(self.root.join("net").join(table)).ok()?;
            socket_inode(&text, local_port, remote_port)
        })?;
        let wanted = format!("socket:[{inode}]");
        std::fs::read_dir(&self.root)
            .ok()?
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .find(|&pid| self.holds(pid, &wanted))
    }

    fn holds(&self, pid: u32, wanted: &str) -> bool {
        let Ok(fds) = std::fs::read_dir(self.of(pid).join("fd")) else {
            return false;
        };
        fds.flatten()
            .any(|fd| std::fs::read_link(fd.path()).is_ok_and(|target| target == Path::new(wanted)))
    }
}

impl ProcessLookup for Proc {
    fn entry(&self, pid: u32) -> Option<ProcessEntry> {
        self.stat(pid).map(|stat| ProcessEntry {
            parent: stat.parent,
            name: stat.comm,
        })
    }

    fn start_time(&self, pid: u32) -> Option<i64> {
        self.stat(pid).map(|stat| stat.started)
    }

    /// The link `exe`. An executable replaced while it runs (an update) reads
    /// `/path (deleted)`: it is still the one it started from.
    fn exe(&self, pid: u32) -> Option<String> {
        let target = std::fs::read_link(self.of(pid).join("exe")).ok()?;
        let text = target.to_string_lossy();
        Some(text.strip_suffix(" (deleted)").unwrap_or(&text).to_string())
    }

    fn command_line(&self, pid: u32) -> Option<String> {
        let raw = std::fs::read(self.of(pid).join("cmdline")).ok()?;
        let parts: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_lines_with_odd_names() {
        let line =
            "4242 (tmux: server) S 1 4242 4242 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 98765 1 2 3";
        assert_eq!(
            parse_stat(line),
            Some(Stat {
                parent: 1,
                comm: "tmux: server".into(),
                started: 98765
            })
        );
        let odd = "7 (a) b (c)) R 3 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0";
        assert_eq!(
            parse_stat(odd).map(|s| (s.comm, s.parent, s.started)),
            Some(("a) b (c)".into(), 3, 55))
        );
        assert_eq!(parse_stat("7 (cut"), None);
        assert_eq!(parse_stat("7 (short) S 1"), None);
    }

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0000000000000000 100 0 0 10 0
   1: 0100007F:C350 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 222 1 0000000000000000 20 4 30 10 -1
   2: 0100007F:1F90 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 333 1 0000000000000000 20 4 30 10 -1
   3: 0A00020F:C351 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 444 1 0000000000000000 20 4 30 10 -1
";

    #[test]
    fn the_socket_of_a_loopback_connection() {
        // Which side: the program's (from 50000 to the hub's 8080), not the hub's.
        assert_eq!(socket_inode(TCP, 0xC350, 0x1F90), Some(222));
        assert_eq!(socket_inode(TCP, 0x1F90, 0xC350), Some(333));
        // From an address that isn't loopback: not a program on this computer.
        assert_eq!(socket_inode(TCP, 0xC351, 0x1F90), None);
        assert_eq!(socket_inode(TCP, 1, 2), None);
        assert_eq!(socket_inode("", 1, 2), None);
    }

    #[test]
    fn ipv6_loopback_and_mapped_ipv4() {
        let words = |bytes: [u8; 16]| -> String {
            bytes
                .chunks(4)
                .map(|c| format!("{:08X}", u32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
                .collect()
        };
        let one = words(std::net::Ipv6Addr::LOCALHOST.octets());
        let mapped = words(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped().octets());
        let other = words(
            "2001:db8::1"
                .parse::<std::net::Ipv6Addr>()
                .unwrap()
                .octets(),
        );
        let table = format!(
            "header\n 0: {one}:C350 {one}:1F90 01 0 0 0 1000 0 555 1\n 1: {mapped}:C351 {mapped}:1F90 01 0 0 0 1000 0 666 1\n 2: {other}:C352 {one}:1F90 01 0 0 0 1000 0 777 1\n"
        );
        assert_eq!(socket_inode(&table, 0xC350, 0x1F90), Some(555));
        assert_eq!(socket_inode(&table, 0xC351, 0x1F90), Some(666));
        assert_eq!(socket_inode(&table, 0xC352, 0x1F90), None);
    }

    #[test]
    fn a_start_as_an_instant() {
        assert_eq!(
            parse_boot_time("cpu 1 2 3\nbtime 1700000000\nprocesses 9\n"),
            Some(1_700_000_000)
        );
        assert_eq!(parse_boot_time("cpu 1 2 3\n"), None);
        let at = instant(1_700_000_000, 250, 100).unwrap();
        assert_eq!(
            at.duration_since(std::time::UNIX_EPOCH).unwrap(),
            std::time::Duration::from_millis(1_700_000_002_500)
        );
        assert_eq!(instant(1, 0, 100), None);
        assert_eq!(instant(1, 5, 0), None);
    }

    /// A `/proc` of its own, with links: only where links can be made without privileges.
    #[cfg(unix)]
    #[test]
    fn processes_and_sockets_from_a_proc_folder() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("clak-procfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let process = |pid: u32, stat: &str, exe: Option<&str>, cmdline: &[u8], fds: &[&str]| {
            let dir = root.join(pid.to_string());
            std::fs::create_dir_all(dir.join("fd")).unwrap();
            std::fs::write(dir.join("stat"), stat).unwrap();
            std::fs::write(dir.join("cmdline"), cmdline).unwrap();
            if let Some(exe) = exe {
                symlink(exe, dir.join("exe")).unwrap();
            }
            for (i, target) in fds.iter().enumerate() {
                symlink(target, dir.join("fd").join(i.to_string())).unwrap();
            }
        };
        process(
            10,
            "10 (bash) S 1 10 10 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 500 0",
            Some("/usr/bin/bash"),
            b"-bash\0",
            &["/dev/pts/0", "socket:[111]"],
        );
        process(
            20,
            "20 (node) S 10 20 10 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 600 0",
            Some("/home/u/.vscode-server/bin/abc/node (deleted)"),
            b"node\0/x/bootstrap-fork\0--type=ptyHost\0",
            &["pipe:[9]", "socket:[222]"],
        );
        // Another user's: stat only.
        process(
            30,
            "30 (sshd) S 1 30 30 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 40 0",
            None,
            b"",
            &[],
        );
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::write(root.join("net/tcp"), TCP).unwrap();
        std::fs::write(root.join("stat"), "cpu 0\nbtime 1700000000\n").unwrap();
        std::fs::create_dir_all(root.join("self")).unwrap();

        let proc = Proc::at(&root);
        assert_eq!(
            proc.entry(20),
            Some(ProcessEntry {
                parent: 10,
                name: "node".into()
            })
        );
        assert_eq!(proc.start_time(20), Some(600));
        assert_eq!(
            proc.exe(20).as_deref(),
            Some("/home/u/.vscode-server/bin/abc/node")
        );
        assert_eq!(proc.exe(30), None);
        assert_eq!(
            proc.command_line(20).as_deref(),
            Some("node /x/bootstrap-fork --type=ptyHost")
        );
        assert_eq!(proc.command_line(30).as_deref(), Some(""));
        assert_eq!(proc.entry(99), None);
        assert_eq!(proc.boot_time(), Some(1_700_000_000));

        assert_eq!(proc.tcp_owner(0xC350, 0x1F90), Some(20));
        // The socket is in the table but nobody we can see holds it.
        assert_eq!(proc.tcp_owner(0x1F90, 0xC350), None);
        assert_eq!(proc.tcp_owner(1, 2), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
