#![allow(unused)]

// prevents console by default
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod sprites;
mod parser;
mod sensing;
mod base;
mod worker;

use std::sync::{mpsc, Arc, RwLock};

use crate::base::MYTHIC_VERSION;

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

fn load_config_and_init_log(file_logging: bool) {

    let default_mll = log::LevelFilter::Warn;
    let default_nlf = 10;

    let path = &base::path().config;
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

    let mut config = match parser::toml::Parser::new(&config_str).parse() {
        Err(e) => {
            // couldn't parse the file, set up default logger, return default config
            base::init_logger(file_logging, default_mll, default_nlf);
            let (buf, span) = parser::lineview(&config_str, e.pos.span);
            base::write_report(|f| e.val.message_with_evidence(
                f, &path.as_rel().to_string_lossy(), e.pos.line, buf, Some(span)
            ));
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
            nlf_ent.val.val.extract::<f32>().map(|&nlf|
                (nlf as i32).clamp(0, 100) as u8
            ).map_err(|e| nlf_ent.val.pos.with(e))
        ).unwrap_or(Ok(default_nlf));

    let mll = max_log_level.unwrap_or(Ok(default_mll)).unwrap_or(default_mll);
    let nlf = num_log_files.unwrap_or(default_nlf);
    base::init_logger(file_logging, mll, nlf);

    // now that we have the logger initialized, we handle errors as usual

    if let Err(e) = max_log_level {
        let (buf, span) = parser::lineview(&config_str, e.pos.span);
        base::write_report(|f| parser::message_with_evidence(
            f, log::Level::Error, &path.as_rel().to_string_lossy(),
            e.pos.line, buf, Some(span), |f|
            write!(f, "value for field 'max-log-files' should be '{}' but found '{}', using default (=warn)", e.val.expected, e.val.found)
        ));
    }

    if let Ok(Err(pos)) = max_log_level {
        let (buf, span) = parser::lineview(&config_str, pos.span);
        base::write_report(|f| parser::message_with_evidence(
            f, log::Level::Error, &path.as_rel().to_string_lossy(),
            pos.line, buf, Some(span), |f|
            write!(f, "value for field 'max-log-files' should be one of [off, error, warn, info, debug, trace], using default (=warn)")
        ));
    }

    if let Err(e) = num_log_files {
        let (buf, span) = parser::lineview(&config_str, e.pos.span);
        base::write_report(|f| parser::message_with_evidence(
            f, log::Level::Error, &path.as_rel().to_string_lossy(),
            e.pos.line, buf, Some(span), |f|
            write!(f, "value for field 'num-log-files' should be '{}' but found '{}', using default (=10)", e.val.expected, e.val.found)
        ));
    }

    let _ = config.pop("online-decoding");

    for ent in config.0 {
        let (buf, span) = parser::lineview(&config_str, ent.key.pos.span);
        base::write_report(|f| parser::message_with_evidence(
            f, log::Level::Error, &path.as_rel().to_string_lossy(),
            ent.key.pos.line, buf, Some(span), |f|
            write!(f, "unrecognized field")
        ));
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

fn main() {
    attach_parent_console();

    let Some(args) = parse_args() else {
        print_help();
        return;
    };

    if args.help {
        print_help();
        return;
    }

    if let Err(e) = base::init_paths(args.app_root) {
        match e {
            base::PathInitError::AppPathResolve =>
                println!("ERROR! Could not resolve app root path, consider providing valid one via cmd arg"),
            base::PathInitError::CouldNotCreate(path, e) =>
                println!("ERROR! Could not create directory '{}' ({e})", path.to_string_lossy()),
            base::PathInitError::ExistingNotCompat(path) =>
                println!("ERROR! Existing '{}' prevents app path initialization, consider removing it", path.to_string_lossy()),
        }
        return;
    }

    if args.info {
        print!(
"mythic v{}.{}
APP_ROOT: {}
",
MYTHIC_VERSION.major, MYTHIC_VERSION.minor,
base::path().root.to_string_lossy(),
        );
        return;
    }

    // should not be dropped till the exit, to keep the lock
    let _lock_file = {
        let path = &base::path().lock;

        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path);

        let f = match f {
            Ok(f) => f,
            Err(e) => {
                println!("ERROR! could not create directory lock file '{path}' ({e})");
                return;
            }
        };

        use fs2::FileExt;

        if let Err(e) = f.try_lock_exclusive() {
            println!("ERROR! could not acquire directory lock file '{path}' ({e})");
            return;
        }

        f
    };

    let _config = load_config_and_init_log(args.filelog);

    let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
    base::write_report(|f| write!(f, "Starting Mythic Monitor v{}.{} log={} at={ts:>011}", MYTHIC_VERSION.major, MYTHIC_VERSION.minor, base::levelf_as_str(log::max_level())));

    { // create an empty file and drops handle immediately
        let path = &base::path().running;
        if let Err(e) = std::fs::File::create(path) {
            log::error!("could not create running file '{path}' ({e})");
        }
    }

    while run() {
        let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
        base::write_report(|f| write!(f, "Restarting Mythic Monitor at={ts:>011}"));
    }

    let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs();
    base::write_report(|f| write!(f, "Exiting Mythic Monitor at={ts:>011}"));
}

// returns if should restart
fn run() -> bool {
    let clipbank = sprites::ClipBank::new();
    let sensors = sensing::Sensors::new();
    let sprites = sprites::Sprites::new();

    let sprites = Arc::new(RwLock::new(sprites));
    let sensors = Arc::new(RwLock::new(sensors));
    let clipbank = Arc::new(RwLock::new(clipbank));

    let (win_tx, win_rx) = mpsc::channel();
    let rdi = worker::RenderDataInterface::new(
        win_rx, sprites.clone(), sensors.clone(), clipbank.clone()
    );
    let render_thread = std::thread::Builder::new()
    .name("render_thread".to_string())
    .spawn(|| {
        // render is not Send, we need to create it inside
        let mut renderer = worker::Renderer::init(rdi).unwrap();
        renderer.run().unwrap();
        // TODO windows error should make others to shutdown
        // currently, aborts the program
    }).unwrap(); // TODO do more mild shutdown

    let (pol_tx, pol_rx) = mpsc::channel();
    let mut poller = worker::SensorPoller::new(pol_rx, sensors.clone());
    poller.load_all(); // plugins better be loaded before watcher.reload_list_toml
    let poller_thread = std::thread::Builder::new()
    .name("poller_thread".to_string())
    .spawn(move || {
        poller.run();
    }).unwrap();

    let mut watcher = worker::DirectoryWatcher::new(
        win_tx, pol_tx, sprites.clone(), sensors.clone(), clipbank.clone()
    );
    let watcher_thread = std::thread::Builder::new()
    .name("watcher_thread".to_string())
    .spawn(move || {
        let list_toml_path = base::path().sprite_list();
        watcher.reload_list_toml(&list_toml_path);
        watcher.run() // returns if we should restart
        // watcher loop exited, the program should shut down
        // dropping watcher will close mpsc channels, making other threads to exit.
    }).unwrap();

    // join returns Err on thread panic. panics will abort the program, so they are never Err.
    let should_restart = watcher_thread.join().unwrap();
    render_thread.join().unwrap();
    poller_thread.join().unwrap();

    let mut sprites = sprites.write().expect("poisoned sprites lock");
    let mut clipbank = clipbank.write().expect("poisoned clipbank lock");
    let mut sensors = sensors.write().expect("poisoned sensors lock");
    sprites.unload(&mut sensors, &mut clipbank);

    should_restart
}