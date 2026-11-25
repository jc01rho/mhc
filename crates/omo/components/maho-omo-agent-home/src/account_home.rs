use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Darwin,
    Windows,
    Other,
}

/// What the OS says about the user running this process; tests pass their own (omo#12978).
pub trait PlatformFacts {
    fn platform(&self) -> Platform;
    fn uid(&self) -> Option<u32>;
    fn env(&self, name: &str) -> Option<String>;
    fn read_file(&self, path: &str) -> Option<String>;
    fn is_file(&self, path: &str) -> bool;
    /// Always called with an absolute executable path, never a bare name a search could resolve.
    fn run(&self, command: &str, args: &[&str]) -> Option<String>;
}

pub struct SystemPlatformFacts;

impl PlatformFacts for SystemPlatformFacts {
    fn platform(&self) -> Platform {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::Darwin
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }

    fn uid(&self) -> Option<u32> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|real| real.parse().ok())
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn read_file(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn run(&self, command: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(command)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The OS user record's home, read from the account database and never from the environment:
/// `HOME` and `USERPROFILE` can name any directory. `None` when the record cannot be read.
pub fn user_record_home(facts: &dyn PlatformFacts) -> Option<String> {
    match facts.platform() {
        Platform::Linux => passwd_home(facts),
        Platform::Darwin => darwin_home(facts),
        Platform::Windows => windows_profile(facts),
        Platform::Other => None,
    }
}

fn is_posix_absolute(path: &str) -> bool {
    path.starts_with('/')
}

fn is_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn is_windows_absolute(path: &str) -> bool {
    is_drive_absolute(path) || path.starts_with('\\') || path.starts_with('/')
}

fn passwd_home(facts: &dyn PlatformFacts) -> Option<String> {
    let uid = facts.uid()?.to_string();
    let passwd = facts.read_file("/etc/passwd")?;
    let home = passwd
        .split('\n')
        .map(|line| line.split(':').collect::<Vec<&str>>())
        .find(|fields| fields.get(2).copied() == Some(uid.as_str()))
        .and_then(|fields| fields.get(5).map(|home| (*home).to_owned()))?;
    is_posix_absolute(&home).then_some(home)
}

/// `id -P` prints the real uid's record in master.passwd form
/// (name:password:uid:gid:class:change:expire:gecos:home:shell), looked up through Open Directory.
fn darwin_home(facts: &dyn PlatformFacts) -> Option<String> {
    let output = facts.run("/usr/bin/id", &["-P"])?;
    let fields: Vec<&str> = output.trim().split(':').collect();
    let expected = facts.uid().map(|uid| uid.to_string());
    if let Some(expected) = &expected
        && fields.get(2).copied() != Some(expected.as_str())
    {
        return None;
    }
    let home = (*fields.get(8)?).to_owned();
    is_posix_absolute(&home).then_some(home)
}

/// The profile directory the registry records for the token's SID; `%USERPROFILE%` only defaults to it.
fn windows_profile(facts: &dyn PlatformFacts) -> Option<String> {
    let whoami = system32_tool(facts, "whoami.exe")?;
    let reg = system32_tool(facts, "reg.exe")?;
    let who = facts.run(&whoami, &["/user", "/fo", "csv", "/nh"])?;
    let sid = find_sid(&who)?;
    let key = format!("HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList\\{sid}");
    let queried = facts.run(&reg, &["query", key.as_str(), "/v", "ProfileImagePath"])?;
    let value = profile_image_path(&queried)?;
    let home = expand_variables(&value, facts);
    (!home.contains('%') && is_windows_absolute(&home)).then_some(home)
}

fn find_sid(text: &str) -> Option<String> {
    let start = text.find("S-1-")?;
    let rest: String = text[start + 4..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    if rest.is_empty() {
        return None;
    }
    Some(format!("S-1-{rest}"))
}

fn profile_image_path(text: &str) -> Option<String> {
    let after = &text[text.find("ProfileImagePath")? + "ProfileImagePath".len()..];
    let after = after.trim_start();
    let after = after
        .strip_prefix("REG_EXPAND_SZ")
        .or_else(|| after.strip_prefix("REG_SZ"))?;
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let value = after.trim_start().lines().next()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn expand_variables(value: &str, facts: &dyn PlatformFacts) -> String {
    let mut expanded = String::new();
    let mut rest = value;
    while let Some(open) = rest.find('%') {
        expanded.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('%') {
            Some(close) if close > 0 => {
                let name = &after[..close];
                match facts.env(name) {
                    Some(resolved) => expanded.push_str(&resolved),
                    None => {
                        expanded.push('%');
                        expanded.push_str(name);
                        expanded.push('%');
                    }
                }
                rest = &after[close + 1..];
            }
            _ => {
                expanded.push('%');
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    expanded
}

/// Windows searches the current directory before PATH for a bare command name, so `whoami` or `reg`
/// would run a copy planted in the repository the agent starts in. Only the copy in
/// `%SystemRoot%\System32` runs, and only when `SystemRoot` is a drive-absolute path without `..`
/// and the tool exists there; otherwise there is no lookup.
fn system32_tool(facts: &dyn PlatformFacts, name: &str) -> Option<String> {
    let root = facts.env("SystemRoot")?;
    if !is_drive_absolute(&root) || root.split(['\\', '/']).any(|segment| segment == "..") {
        return None;
    }
    let tool = format!("{}\\System32\\{name}", root.trim_end_matches(['\\', '/']));
    facts.is_file(&tool).then_some(tool)
}

#[cfg(test)]
pub(crate) mod fake {
    use super::{Platform, PlatformFacts};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    pub(crate) struct FakeFacts {
        pub platform: Platform,
        pub uid: Option<u32>,
        pub env: BTreeMap<String, String>,
        pub files: BTreeMap<String, String>,
        pub outputs: BTreeMap<String, String>,
        pub ran: RefCell<Vec<String>>,
    }

    impl FakeFacts {
        pub(crate) fn new(platform: Platform, uid: Option<u32>) -> Self {
            Self {
                platform,
                uid,
                env: BTreeMap::new(),
                files: BTreeMap::new(),
                outputs: BTreeMap::new(),
                ran: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn linux_passwd(uid: u32, passwd: &str) -> Self {
            let mut facts = Self::new(Platform::Linux, Some(uid));
            facts
                .files
                .insert("/etc/passwd".to_owned(), passwd.to_owned());
            facts
        }
    }

    impl PlatformFacts for FakeFacts {
        fn platform(&self) -> Platform {
            self.platform
        }
        fn uid(&self) -> Option<u32> {
            self.uid
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env.get(name).cloned()
        }
        fn read_file(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn is_file(&self, path: &str) -> bool {
            self.files.contains_key(path)
        }
        fn run(&self, command: &str, args: &[&str]) -> Option<String> {
            let line = format!("{command} {}", args.join(" "));
            self.ran.borrow_mut().push(line.clone());
            self.outputs.get(&line).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeFacts;
    use super::*;

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/bash\nalice:x:1000:1000::/home/alice:/bin/sh\n";

    #[test]
    fn linux_reads_the_passwd_entry_of_the_uid() {
        let facts = FakeFacts::linux_passwd(1000, PASSWD);
        assert_eq!(user_record_home(&facts).as_deref(), Some("/home/alice"));
        let other = FakeFacts::linux_passwd(777, PASSWD);
        assert_eq!(user_record_home(&other), None);
        let relative = FakeFacts::linux_passwd(1000, "alice:x:1000:1000::home/alice:/bin/sh\n");
        assert_eq!(user_record_home(&relative), None);
        let unreadable = FakeFacts::new(Platform::Linux, Some(1000));
        assert_eq!(user_record_home(&unreadable), None);
        let no_uid = FakeFacts::new(Platform::Linux, None);
        assert_eq!(user_record_home(&no_uid), None);
    }

    fn darwin(uid: Option<u32>) -> FakeFacts {
        let mut facts = FakeFacts::new(Platform::Darwin, uid);
        facts.outputs.insert(
            "/usr/bin/id -P".to_owned(),
            "alice:********:501:20::0:0:Alice:/Users/alice:/bin/zsh\n".to_owned(),
        );
        facts
    }

    #[test]
    fn darwin_reads_the_home_field_of_id_p_looked_up_by_uid() {
        assert_eq!(
            user_record_home(&darwin(Some(501))).as_deref(),
            Some("/Users/alice")
        );
        assert_eq!(
            user_record_home(&darwin(None)).as_deref(),
            Some("/Users/alice")
        );
        assert_eq!(user_record_home(&darwin(Some(502))), None);
        assert_eq!(
            user_record_home(&FakeFacts::new(Platform::Darwin, Some(501))),
            None
        );
    }

    const WHOAMI: &str = "C:\\Windows\\System32\\whoami.exe /user /fo csv /nh";
    const REG: &str = "C:\\Windows\\System32\\reg.exe query HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList\\S-1-5-21-1-2-3-1001 /v ProfileImagePath";

    fn windows(system_root: Option<&str>) -> FakeFacts {
        let mut facts = FakeFacts::new(Platform::Windows, None);
        facts.env.insert("SystemDrive".to_owned(), "C:".to_owned());
        facts
            .env
            .insert("USERPROFILE".to_owned(), "D:\\scratch".to_owned());
        if let Some(root) = system_root {
            facts.env.insert("SystemRoot".to_owned(), root.to_owned());
        }
        for tool in ["whoami.exe", "reg.exe"] {
            facts
                .files
                .insert(format!("C:\\Windows\\System32\\{tool}"), String::new());
        }
        facts.outputs.insert(
            WHOAMI.to_owned(),
            "\"PC\\alice\",\"S-1-5-21-1-2-3-1001\"\r\n".to_owned(),
        );
        facts.outputs.insert(
            REG.to_owned(),
            "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\ProfileList\\S-1-5-21-1-2-3-1001\r\n    ProfileImagePath    REG_EXPAND_SZ    %SystemDrive%\\Users\\alice\r\n\r\n".to_owned(),
        );
        facts
    }

    #[test]
    fn windows_reads_the_registry_profile_path_with_system_variables_expanded() {
        let facts = windows(Some("C:\\Windows"));
        assert_eq!(
            user_record_home(&facts).as_deref(),
            Some("C:\\Users\\alice")
        );
        assert_eq!(
            *facts.ran.borrow(),
            vec![WHOAMI.to_owned(), REG.to_owned()],
            "only the System32 copies run, never a bare name"
        );
    }

    #[test]
    fn windows_without_a_usable_system_root_or_tool_runs_nothing() {
        for root in [None, Some("Windows"), Some("C:\\Windows\\..\\Temp")] {
            let facts = windows(root);
            assert_eq!(user_record_home(&facts), None, "{root:?}");
            assert!(facts.ran.borrow().is_empty(), "{root:?}");
        }
        let mut missing_tool = windows(Some("C:\\Windows"));
        missing_tool.files.remove("C:\\Windows\\System32\\reg.exe");
        assert_eq!(user_record_home(&missing_tool), None);
        assert!(missing_tool.ran.borrow().is_empty());
    }

    #[test]
    fn windows_with_no_sid_or_an_unexpanded_variable_names_no_home() {
        let mut no_sid = windows(Some("C:\\Windows"));
        no_sid
            .outputs
            .insert(WHOAMI.to_owned(), "no sid here".to_owned());
        assert_eq!(user_record_home(&no_sid), None);
        let mut unexpanded = windows(Some("C:\\Windows"));
        unexpanded.env.remove("SystemDrive");
        assert_eq!(user_record_home(&unexpanded), None);
    }

    #[test]
    fn a_platform_without_a_user_record_lookup_names_none() {
        assert_eq!(
            user_record_home(&FakeFacts::new(Platform::Other, Some(1))),
            None
        );
    }
}
