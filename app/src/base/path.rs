use std::sync::OnceLock;
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

    pub fn join(&self, path: impl AsRef<Path>) -> AppPath {
        AppPath(self.0.join(path))
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
        write!(f, "{}", self.as_rel().to_string_lossy())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPathAnalisys<'p> {
    Sprite(&'p Path), // .toml file in toml dir
    Clip  (&'p Path), // .webp file in sprite dir
    Plugin(&'p Path), // .dll/.so file in plugin dir
    #[cfg(debug_assertions)]
    PluginDebug,      // debug.toml file in plugin dir
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

        #[cfg(debug_assertions)]
        {
            if stripped_path == Path::new("debug.toml") {
                return AppPathAnalisys::PluginDebug;
            }
        }

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

pub fn app_paths() -> &'static AppPathGlobal {
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