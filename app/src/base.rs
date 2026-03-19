use std::io::{LineWriter, Write, stderr};
use std::sync::{Mutex, OnceLock};

struct Logger(OnceLock<Mutex<Box<dyn Write + Send>>>);
static LOGGER: Logger = Logger(OnceLock::new());

pub fn prepare_log_file() -> Result<impl Write + Send, std::io::Error> {
    let path = &get_app_path_global().log;

    if path.exists() {
        // Creation of files in windows have some... strange behavior..
        // After moving old log and creating new one, the new log (now.log) creation time
        // is kept from the old log, which in tern, causes overwriting the old log on next log file preparation.
        // Modified date does not have such quirks for unexplained reason.
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let ts = modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                // unix epoch time in seconds will starts to have 11 digits from 2286-11-20
                let new_path = path.parent().unwrap().slash(format!("log-{ts:>013}.log"));
                let _ = std::fs::rename(path, new_path);
                // ignoring error. if moving fails, it gets overwritten
            }
        }
    }

    std::fs::File::create(path).map(|f| LineWriter::new(f))
}

// returns true if success
fn cleanup_log_files(max_num: usize) -> bool {
    let path = &get_app_path_global().log;
    let path = path.parent().unwrap();

    let Ok(logs) = std::fs::read_dir(&path) else { return false; };
    let mut logs = logs.filter_map(|entry| {
        let entry = entry.ok()?;
        if ! entry.file_type().map(|t| t.is_file()).unwrap_or(false) { return None; }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else { return None; };
        if ! name.starts_with("log-") { return None; }
        if ! name.ends_with(".log") { return None; }
        Some(file_name)
    }).collect::<Vec<_>>();
    // collect files with utf8 name in "log-*.log" format

    logs.sort();
    logs.reverse();

    while logs.len() > max_num {
        let file_name = logs.pop().unwrap();
        let file_path = path.0.join(file_name);
        _ = std::fs::remove_file(file_path);
    }

    true
}

pub fn init_logger(file_logging: bool, max_level: log::LevelFilter, num_logs: u8) {
    let writer: Box<dyn Write + Send> = if file_logging {
        match prepare_log_file() {
            Ok(writer) => {
                cleanup_log_files(num_logs as usize);
                Box::new(writer)
            }

            Err(e) => {
                println!("ERROR! could not prepare log file at '{}' ({e})", get_app_path_global().log.to_string_lossy());
                Box::new(stderr()) as Box<dyn std::io::Write + Send>
            }
        }
    } else {
        Box::new(stderr())
    };

    LOGGER.0.set(Mutex::new(writer)).ok().unwrap();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(max_level);
}

impl log::Log for Logger {
    fn enabled(&self, _meta: &log::Metadata) -> bool {
        true //set_max_level already handles level filtering. I don't have to do anything here.
    }

    fn log(&self, record: &log::Record) {
        if ! self.enabled(record.metadata()) { return; }

        if let Some(lock) = self.0.get() {
            // if failed_before { return; }
            'fail: {
                let Ok(mut writer) = lock.lock() else { break 'fail };
                if record.metadata().target() != "__user" {
                    let Ok(_) = writer.write_fmt(format_args!("{}: ", level_as_str(record.level()))) else { break 'fail };
                }
                let Ok(_) = writer.write_fmt(*record.args()) else { break 'fail };
                let Ok(_) = writer.write_all(b"\n") else { break 'fail };
                let Ok(_) = writer.flush() else { break 'fail };
                return;
            }
            // TODO handle failure?
        }
    }

    fn flush(&self) {
        if let Some(lock) = self.0.get() {
            if let Ok(mut writer) = lock.lock() {
                _ = writer.flush();
            }
        }
    }
}

// same as level.as_str but in lowercase
static LOG_LEVEL_NAMES: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];
pub fn level_as_str(level: log::Level) -> &'static str{
    LOG_LEVEL_NAMES[level as usize]
}
pub fn levelf_as_str(level: log::LevelFilter) -> &'static str{
    LOG_LEVEL_NAMES[level as usize]
}

pub fn log_report<T>(v: T) where T: std::fmt::Display {
    log::error!(target: "__user", "{}", v);
}

struct WriterWrapper<F>(F);
impl<F> std::fmt::Display for WriterWrapper<F> where F: Fn(&mut std::fmt::Formatter) -> std::fmt::Result {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0(f)
    }
}
pub fn write_report<F>(f: F) where F: Fn(&mut std::fmt::Formatter) -> std::fmt::Result {
    log::error!(target: "__user", "{}", WriterWrapper(f));
}


/// Path

use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AppPathGlobal {
    pub sprite: AppPath,
    pub plugin: AppPath,
    pub misc:   AppPath,
    pub running:AppPath,
    pub config: AppPath,
    pub log:    AppPath,
    pub root:   AppPath,
    pub templ:  AppPath, // temp directory for plugin
    pub lock:   AppPath,
}

macro_rules! create_or_verify_dir {
    ($path:expr) => {
        if ! $path.exists() {
            if let Err(e) = std::fs::create_dir_all(&$path) {
                return Err(PathInitError::CouldNotCreate($path, e));
            }
        }
        if ! $path.is_dir() { return Err(PathInitError::ExistingNotCompat($path)); }
    };
}

impl AppPathGlobal {
    pub fn init(root: PathBuf) -> Result<Self, PathInitError> {
        let root   = AppPath(root);
        create_or_verify_dir!(root.0);
        let templ  = AppPath(root.0.join(".templ"));
        _ = std::fs::remove_dir_all(&templ.0);
        create_or_verify_dir!(templ.0);
        let sprite = AppPath(root.0.join("sprites"));
        create_or_verify_dir!(sprite.0);
        let plugin = AppPath(root.0.join("plugins"));
        create_or_verify_dir!(plugin.0);
        let misc   = AppPath(root.0.join("misc"));
        create_or_verify_dir!(misc.0);
        let running= AppPath(misc.0.join("running"));
        let config = AppPath(misc.0.join("config.toml"));
        let log    = AppPath(misc.0.join("now.log"));
        let lock   = AppPath(misc.0.join(".lock"));

        Ok(Self { sprite, plugin, misc, running, config, log, root, templ, lock, })
    }

    fn conv(base: &Path, path: impl AsRef<Path>) -> AppPath {
        let path = path.as_ref();
        AppPath(
            if path.is_absolute() {
                if ! path.starts_with(base) {
                    panic!("cannot convert absolute path '{}' into AppPath", path.to_string_lossy());
                }
                path.to_path_buf()
            } else {
                base.join(path)
            }
        )
    }

    pub fn plugin(&self, path: impl AsRef<Path>) -> AppPath {
        AppPathGlobal::conv(&self.plugin.0, path)
    }

    pub fn sprite(&self, path: impl AsRef<Path>) -> AppPath {
        AppPathGlobal::conv(&self.sprite.0, path)
    }

    pub fn misc(&self, path: impl AsRef<Path>) -> AppPath {
        AppPathGlobal::conv(&self.misc.0, path)
    }

    pub fn templ(&self, path: impl AsRef<Path>) -> AppPath {
        AppPathGlobal::conv(&self.templ.0, path)
    }

    pub fn sprite_list(&self) -> AppPath {
        self.sprite("list.toml")
    }
}

/// PathBuf that is contained in the root directory.
/// Note that it does not handle '..' or symlinks so the path may escape the root directory.
/// In which case, watcher won't be able to detect update for those files.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppPath(PathBuf);

impl AppPath {
    pub fn as_pathbuf(&self) -> &PathBuf {
        &self.0
    }

    pub fn as_rel(&self) -> &Path {
        let root = &get_app_path_global().root;
        self.0.strip_prefix(root).unwrap()
    }

    pub fn parent(&self) -> Option<AppPath> {
        let root = &get_app_path_global().root;
        let parent = self.0.parent()?;
        if ! parent.starts_with(root) {
            return None;
        }
        Some(AppPath(parent.to_path_buf()))
    }

    pub fn slash(mut self, path: impl AsRef<Path>) -> AppPath {
        if path.as_ref().is_absolute() { panic!() }
        self.0.push(path);
        self
    }
}

impl AsRef<Path> for AppPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for AppPath {
    type Target = Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TryFrom<PathBuf> for AppPath {
    type Error = PathBuf;
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        let root = &get_app_path_global().root;
        if path.starts_with(root) {
            Ok(AppPath(path))
        } else {
            Err(path)
        }
    }
}

impl std::fmt::Display for AppPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}'", self.as_rel().to_string_lossy())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPathAnalisys<'p> {
    Sprite(&'p Path), // .toml file in toml dir
    Clip  (&'p Path), // .webp file in sprite dir
    Plugin(&'p Path), // .dll/.so file in plugin dir
    TempL (&'p Path), // loaded plugin files
    SpriteList, // sprites/list.toml
    Running,    // misc/running
    Log,        // misc/*.log
    Lock,       // misc/lock
    Unknown,
}

pub fn analize_path(path: &AppPath) -> AppPathAnalisys<'_> {
    let glob = get_app_path_global();

    if let Ok(stripped_path) = path.strip_prefix(&glob.sprite) {
        if ! path.is_file() { return AppPathAnalisys::Unknown; }

        match path.extension().map(|s| s.to_str()).flatten() {
            Some("webp") => AppPathAnalisys::Clip(stripped_path),

            Some("toml") => {
                if stripped_path == Path::new("list.toml") {
                    AppPathAnalisys::SpriteList
                } else {
                    AppPathAnalisys::Sprite(stripped_path)
                }
            }
            _ => AppPathAnalisys::Unknown,
        }

    } else if let Ok(stripped_path) = path.strip_prefix(&glob.plugin) {
        // plugin

        #[cfg(target_os = "windows")]
        {
            if path.extension() == Some(std::ffi::OsStr::new("dll")) {
                AppPathAnalisys::Plugin(stripped_path)
            } else { AppPathAnalisys::Unknown }
        }

        #[cfg(target_os = "linux")]
        {
            if path.extension() == Some(std::ffi::OsStr::new("so")) {
                AppPathAnalisys::Plugin(stripped_path)
            } else { AppPathAnalisys::Unknown }
        }

    } else if let Ok(stripped_path) = path.strip_prefix(&glob.templ) {
        // loaded plugin in temporary dir

        #[cfg(target_os = "windows")]
        {
            if path.extension() == Some(std::ffi::OsStr::new("dll")) {
                AppPathAnalisys::TempL(stripped_path)
            } else { AppPathAnalisys::Unknown }
        }

        #[cfg(target_os = "linux")]
        {
            if path.extension() == Some(std::ffi::OsStr::new("so")) {
                AppPathAnalisys::TempL(stripped_path)
            } else { AppPathAnalisys::Unknown }
        }


    } else if let Ok(stripped_path) = path.strip_prefix(&glob.misc) {
        if stripped_path == Path::new("running") {
            AppPathAnalisys::Running
        } else if stripped_path == Path::new("lock") {
            AppPathAnalisys::Lock
        } else if stripped_path.extension() == Some(std::ffi::OsStr::new("log")) {
            AppPathAnalisys::Log
        } else { AppPathAnalisys::Unknown }

    } else {
        AppPathAnalisys::Unknown
    }
}

static PATHS: OnceLock<AppPathGlobal> = OnceLock::new();

pub enum PathInitError {
    AppPathResolve,
    CouldNotCreate(PathBuf, std::io::Error),
    ExistingNotCompat(PathBuf),
}

// must be initialized before log_init
pub fn init_paths(app_root: Option<PathBuf>) -> Result<(), PathInitError> {
    let app_root = 'app_root: {
        if let Some(app_root) = app_root {
            break 'app_root app_root;
        }

        if let Some(path) = option_env!("MYTHIC_ROOT") {
            break 'app_root PathBuf::from(path);
        }

        #[cfg(not(debug_assertions))]
        if let Some(data_dir) = dirs::data_dir() {
            break 'app_root data_dir.slash("mythic");
        }

        #[cfg(debug_assertions)]
        if let Some(example) = std::path::absolute("example").ok() {
            break 'app_root example;
        }

        return Err(PathInitError::AppPathResolve);
    };

    let apg = AppPathGlobal::init(app_root)?;

    PATHS.set(apg).expect("initialize AppPathGlobal twice");
    Ok(())
}

fn get_app_path_global() -> &'static AppPathGlobal {
    PATHS.get().unwrap()
}

pub fn path() -> &'static AppPathGlobal {
    get_app_path_global()
}

pub trait PathBufJoinChain {
    fn slash(self, path: impl AsRef<Path>) -> Self;
}

impl PathBufJoinChain for PathBuf {
    fn slash(mut self, path: impl AsRef<Path>) -> Self {
        self.push(path);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoSize {
    pub width: Option<usize>,
    pub height: Option<usize>,
}

impl AutoSize {
    pub fn new(width: Option<usize>, height: Option<usize>) -> Self {
        Self { width, height }
    }

    pub fn complete(&self, width: usize, height: usize) -> (usize, usize) {
        match (self.width, self.height) {
            (None, None) =>
                (width, height),
            (Some(w), None) =>
                (w, w * height / width),
            (None, Some(h)) =>
                (h * width / height, h),
            (Some(w), Some(h)) =>
                (w, h)
        }
    }

    pub fn is_complete(&self) -> bool {
        self.width.is_some() && self.height.is_some()
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}


pub fn o3_hungarian(n: usize, m: usize, cost: impl Fn (usize, usize) -> i32) -> (i32, Vec<usize>) {
    assert!(n <= m);
    const MAX: i32 = 1_000_000_000;
    let mut price_l = vec![0; n+1];
    let mut price_r = vec![0; m+1];
    // let mut match_r2l = vec![None; m+1];
    // let mut way = vec![None; m+1];
    let mut match_r2l = vec![0; m+1];
    let mut way = vec![0; m+1];

    for i in 1 ..= n {
        match_r2l[0] = i;
        let mut j0 = 0;
        let mut min_r = vec![MAX; m+1];
        let mut used = vec![false; m+1];
        loop {
            used[j0] = true;
            let i0 = match_r2l[j0];
            let mut delta = MAX;
            let mut j1 = 0;
            for j in 1 ..= m {
                if !used[j] {
                    let cur = (cost(i0, j)) - price_l[i0] - price_r[j];
                    if cur < min_r[j] {
                        min_r[j] = cur; way[j] = j0;
                    }
                    if min_r[j] < delta {
                        delta = min_r[j];  j1 = j;
                    }
                }
            }
            for j in 0 ..= m {
                if used[j] {
                    price_l[match_r2l[j]] += delta; price_r[j] -= delta;
                } else {
                    min_r[j] -= delta;
                }
            }
            j0 = j1;
            if match_r2l[j0] == 0 { break; }
        }

        loop {
            let j1 = way[j0];
            match_r2l[j0] = match_r2l[j1];
            j0 = j1;
            if j0 == 0 { break; }
        }
    }

  // let mut ans = vec![0; n+1];
  // for j in 1 ..= m {
  //     ans[match_r2l[j]] = j;
  // }

  (-price_r[0], match_r2l)
}


pub struct Version {
    pub major: u8,
    pub minor: u8,
}

pub const MYTHIC_VERSION: Version = Version {
    major: parse_u8(env!("CARGO_PKG_VERSION_MAJOR")),
    minor: parse_u8(env!("CARGO_PKG_VERSION_MINOR")),
};

const fn parse_u8(s: &str) -> u8 {
    let mut r = 0;
    let s = s.as_bytes();
    if s.len() > 0 { r += (s[0] - b'0') * 1; }
    if s.len() > 1 { r += (s[1] - b'0') * 10; }
    if s.len() > 2 { r += (s[2] - b'0') * 100; }
    r
}

pub fn is_version_compatible(spec: &str) -> Option<bool> {

    fn parse_version(input: &str) -> Option<(u64, Option<u64>)> {
        let mut parts = input.trim().split('.');
        let major = parts.next()?.trim().parse::<u64>().ok()?;
        let minor_opt = if let Some(m) = parts.next() {
            let m = m.trim();
            if m.is_empty() { return None; }
            Some(m.parse::<u64>().ok()?)
        } else { None };
        if parts.next().is_some() { return None; }
        Some((major, minor_opt))
    }

    let cur_major = MYTHIC_VERSION.major as u64;
    let cur_minor = MYTHIC_VERSION.minor as u64;

    let spec = spec.trim();
    if spec.is_empty() { return Some(true); }

    let (op, rest) =
    if let Some(s) = spec.strip_prefix('=') {
        ('=', s.trim())
    } else if let Some(s) = spec.strip_prefix('^') {
        ('^', s.trim())
    } else {
        ('^', spec)
    };

    let (req_major, req_minor_opt) = parse_version(rest)?;

    match op {
        '=' => {
            if let Some(req_minor) = req_minor_opt {
                Some(cur_major == req_major && cur_minor == req_minor)
            } else {
                Some(cur_major == req_major)
            }
        }
        '^' => {
            if cur_major != req_major {
                return Some(false);
            }

            if let Some(req_minor) = req_minor_opt {
                Some(cur_minor >= req_minor)
            } else {
                Some(true)
            }
        }
        _ => unreachable!(),
    }
}