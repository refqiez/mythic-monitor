pub mod animator;
use animator::Animator;

pub mod watcher;
use watcher::DirectoryWatcher;

pub mod poller;
use poller::SensorPoller;

pub mod decoder;
use decoder::ClipsDecoder;

use std::sync::{mpsc, Arc, RwLock};

use crate::{
    base::{self, AppPath, MYTHIC_VERSION, PathInitError, app_paths, init_paths, log_user},
    parser::{self, Pos, WithPos, toml::{ExtractError, ParseError}},
    sensing::{OpaqueError, Sensors},
    sprites::{ClipBank, ClipId, Sprites},
};

pub enum SensingUpdate {
    #[cfg(debug_assertions)]
    PluginDebugUpdage,
}

#[derive(Debug)]
pub enum AnimatorUpdate {
    UpdateQueued,
}

pub enum DecoderUpdate {
    // NewClip(ClipId),
    Rescan, // from Animator::apply_sprite_update
    Advanced(ClipId), // from Animator::redraw_sprite
}

// TODO should be moved to logger?
pub fn report_opaque_error(name: &str, task: &str, e: OpaqueError) {
    log_user!("plugin '{name}' reported error during {task}: {e}");
}

fn report_opaque_error_once(name: &str, task: &str, e: OpaqueError) {
    log_user!("plugin '{name}' reported error during {task}: {e}; subsequent errors of this type are silenced");
}

fn attach_parent_console() -> bool {

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        unsafe {
            // Try to inherit console from the parent.
            // Fails if there's no parent, parent does not have console, or already have console.
            // Allow printing to console when manually run from the terminal.
            let attached = AttachConsole(ATTACH_PARENT_PROCESS);
            attached.is_ok()
        }
    }

    #[cfg(target_os = "linux")]
    {
        // TODO
    }
}

pub enum AppInitReportKind {
    ConfigParse(ParseError),
    MaxLogLevelType(ExtractError),
    MaxLogLevelValue,
    NumLogFiles(ExtractError),
    UnrecognizedField,
}

pub struct AppInitReport<'src> {
    pub file: &'src AppPath,
    pub src: &'src str,
    pub pos: Pos,
    pub kind: AppInitReportKind,
}

fn load_config_and_init_log(file_logging: bool) -> () {

    let default_mll = log::LevelFilter::Warn;
    let default_nlf = 10;

    let path = &app_paths().config;
    if ! path.is_file() {
        base::init_logger(file_logging, default_mll, default_nlf);
        return (); // no config file. use defaults
    }
    // from this point, we know the user tried to provide config but it didnt went well.

    let config_str = match std::fs::read_to_string(path) {
        Err(e) => {
            // couldn't read the file, set up default logger, return default config
            base::init_logger(file_logging, default_mll, default_nlf);
            log::error!("could not load config file '{path}' ({e})");
            return ();
        }

        Ok(config) => config,
    };

    let report_error = |e: WithPos<AppInitReportKind>| {
        log_user!("{}", AppInitReport { file: path, src: &config_str, pos: e.pos, kind: e.val });
    };

    let mut config = match parser::toml::Parser::new(&config_str).parse() {
        Err(e) => {
            // couldn't parse the file, set up default logger, return default config
            base::init_logger(file_logging, default_mll, default_nlf);
            report_error(e.map(AppInitReportKind::ConfigParse));
            return ();
        }
        Ok(config) => config,
    };

    let max_log_level =
        config.pop("max-log-level").map(|mll_ent|
            mll_ent.val.val.extract::<&str>().map(|mll|
                log::LevelFilter::iter().find(|l| l.as_str().eq_ignore_ascii_case(mll))
                .ok_or(mll_ent.val.pos)
            ).map_err(|e| mll_ent.val.pos.with(e))
        ).unwrap_or(Ok(Ok(default_mll)));

    let num_log_files =
        config.pop("num-log-files").map(|nlf_ent|
            nlf_ent.val.val.extract::<f64>().map(|&nlf|
                (nlf as i32).clamp(0, 100) as u8
            ).map_err(|e| nlf_ent.val.pos.with(e))
        ).unwrap_or(Ok(default_nlf));

    let mll = max_log_level.unwrap_or(Ok(default_mll)).unwrap_or(default_mll);
    let nlf = num_log_files.unwrap_or(default_nlf);
    base::init_logger(file_logging, mll, nlf);

    // now that we have the logger initialized, we handle errors as usual

    if let Err(e) = max_log_level {
        report_error(e.map(AppInitReportKind::MaxLogLevelType));
    }

    if let Ok(Err(pos)) = max_log_level {
        report_error(pos.with(AppInitReportKind::MaxLogLevelValue));
    }

    if let Err(e) = num_log_files {
        report_error(e.map(AppInitReportKind::NumLogFiles));
    }

    let _ = config.pop("online-decoding");

    for ent in config.0 {
        report_error(ent.key.pos.with(AppInitReportKind::UnrecognizedField));
    }
}

struct Args {
    app_root: Option<std::path::PathBuf>,
    info: bool,
    help: bool,
    filelog: bool,
}

fn parse_args() -> Option<Args> {
    let mut args = Args {
        app_root: None,
        info: false,
        help: false,
        #[cfg(debug_assertions)]
        filelog: false,
        #[cfg(not(debug_assertions))]
        filelog: true
    };

    for arg in std::env::args_os().skip(1) {
        if arg.as_encoded_bytes()[0] == b'-' {
            if arg == "--info" {
                args.info = true;
            } else if arg == "--help" {
                args.help = true;
            } else if arg == "--nofilelog" {
                args.filelog = false;
            } else {
                println!("ERROR: Unrecognized argument {}", arg.to_string_lossy());
                return None;
            }

        } else if args.app_root.is_none() {
            args.app_root = Some(std::path::PathBuf::from(arg))

        } else {
            println!("ERROR: Too many arguments {}", arg.to_string_lossy());
            return None;
        }
    }

    Some(args)
}

fn print_help() {
    print!(
"USAGE: mythic [APP_ROOT_PATH] [--info] [--help]
    APP_ROOT: override app root path
    --help: print this help message and exit;
    --info: initialize app path, print app info and exit
    --nofilelog: write logs to stdout, instead of log file
");
}

pub fn init() -> Option<std::fs::File> {
    attach_parent_console();

    let Some(args) = parse_args() else {
        print_help();
        return None;
    };

    if args.help {
        print_help();
        return None;
    }

    if let Err(e) = init_paths(args.app_root) {
        match e {
            PathInitError::AppPathResolve =>
                println!("ERROR! Could not resolve app root path, consider providing valid one via cmd arg"),
            PathInitError::CouldNotCreate(path, e) =>
                println!("ERROR! Could not create directory '{}' ({e})", path.to_string_lossy()),
            PathInitError::ExistingNotCompat(path) =>
                println!("ERROR! Existing '{}' prevents app path initialization, consider removing it", path.to_string_lossy()),
        }
        return None;
    }

    if args.info {
        print!("\
mythic v{}.{}
APP_ROOT: {}
",
            MYTHIC_VERSION.major, MYTHIC_VERSION.minor,
            app_paths().root.to_string_lossy(),
        );
        return None;
    }

    // should not be dropped till the exit, to keep the lock
    let lock_file = {
        let path = &app_paths().lock;

        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path);

        let f = match f {
            Ok(f) => f,
            Err(e) => {
                println!("ERROR! could not create directory lock file '{path}' ({e})");
                return None;
            }
        };

        use fs2::FileExt;

        if let Err(e) = f.try_lock_exclusive() {
            println!("ERROR! could not acquire directory lock file '{path}' ({e})");
            return None;
        }

        f
    };

    let _config = load_config_and_init_log(args.filelog);

    { // create an empty file and drops handle immediately
        let path = &app_paths().running;
        if let Err(e) = std::fs::File::create(path) {
            log::error!("could not create running file '{path}' ({e})");
            return None;
        }
    }

    return Some(lock_file);
}

// returns if should restart
pub fn run() -> bool {
    let clipbank = ClipBank::new();
    let sensors = Sensors::new();
    let sprites = Sprites::new();

    let sprites = Arc::new(RwLock::new(sprites));
    let sensors = Arc::new(RwLock::new(sensors));
    let clipbank = Arc::new(RwLock::new(clipbank));

    // spawn worker threads

    // ClipsDecoder
    let (dec_tx, dec_rx) = mpsc::channel();
    let mut decoder = ClipsDecoder::new(clipbank.clone(), dec_rx);
    let decoder_thread = std::thread::Builder::new()
    .name("decoder_thread".to_string())
    .spawn(move || {
        decoder.run()
    }).unwrap();

    // Animator
    let (anim_tx, anim_rx) = mpsc::channel();
    let cloned_arcs = (sprites.clone(), sensors.clone(), clipbank.clone(), dec_tx.clone());
    let anim_thread = std::thread::Builder::new()
    .name("anim_thread".to_string())
    .spawn(|| {
        // render is not Send (since Windows window APIs are expected to be contained in a thread), we need to create it inside
        let mut animator = Animator::init(cloned_arcs.0, cloned_arcs.1, cloned_arcs.2,anim_rx, cloned_arcs.3).unwrap();
        animator.run().unwrap();
        // TODO windows error should make others to shutdown
        // currently, aborts the program
    }).unwrap(); // TODO do more mild shutdown

    // Poller
    let (pol_tx, pol_rx) = mpsc::channel();
    let mut poller = SensorPoller::new(pol_rx, sensors.clone());
    poller.load_all(); // plugins better be loaded before watcher.reload_list_toml
    let poller_thread = std::thread::Builder::new()
    .name("poller_thread".to_string())
    .spawn(move || {
        poller.run();
    }).unwrap();

    // DirectoryWatcher
    let mut watcher = DirectoryWatcher::new(anim_tx, pol_tx, dec_tx, sprites.clone(), sensors.clone(), clipbank.clone());
    let watcher_thread = std::thread::Builder::new()
    .name("watcher_thread".to_string())
    .spawn(move || {
        let list_toml_path = app_paths().sprite_list();
        watcher.reload_list_toml(&list_toml_path);
        watcher.run() // returns if we should restart
        // watcher loop exited, the program should shut down
        // dropping watcher will close mpsc channels, making other threads to exit.
    }).unwrap();


    // join and release resources

    // join returns Err on thread panic. panics will abort the program, so they are never Err.
    let should_restart = watcher_thread.join().unwrap();
    anim_thread.join().unwrap();
    poller_thread.join().unwrap();
    decoder_thread.join().unwrap();

    let mut sprites = sprites.write().expect("poisoned sprites lock");
    let mut clipbank = clipbank.write().expect("poisoned clipbank lock");
    let mut sensors = sensors.write().expect("poisoned sensors lock");
    sprites.unload(&mut sensors, &mut clipbank);

    for (name, e) in sensors.destroy_all() {
        // FIXME should be done in poller? we are doing it here to ensure every thing referencing sensors are not running anymore.
        report_opaque_error(&name, "destroy", e.as_ref());
    }

    should_restart
}