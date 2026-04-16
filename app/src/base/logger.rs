use crate::parser::{lineview, message_with_evidence, toml, Span};
use crate::base::{app_paths, MYTHIC_VERSION};

use std::io::{LineWriter, Write, stderr};
use std::sync::{Mutex, OnceLock};

use std::fmt::{Formatter, Display};
type FmtRet = std::fmt::Result;

struct Logger(OnceLock<Mutex<Box<dyn Write + Send>>>);
static LOGGER: Logger = Logger(OnceLock::new());

fn prepare_log_file() -> Result<impl Write + Send, std::io::Error> {
    let path = &app_paths().log;

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
    let path = &app_paths().log;
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
        let file_path = path.join(file_name);
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
                println!("ERROR! could not prepare log file at '{}' ({e})", app_paths().log.to_string_lossy());
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
// pub fn log_report<T>(v: T, ba:bool) where T: std::fmt::Display {
//     log::error!(target: "__user", "{}", v);
// }

// #[macro_export]
macro_rules! log_user {
    ($($arg:tt)*) => {{
        log::error!(target: "__user", $($arg)*);
    }};
}
pub(crate) use log_user;

pub struct WriterWrapper<F>(pub F);

impl<F> Display for WriterWrapper<F>
where F: Fn(&mut Formatter<'_>) -> FmtRet {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtRet {
        (self.0)(f)
    }
}

fn general_schema_error_message_with_evidence(
    f: &mut std::fmt::Formatter,
    file: &str,
    lineno: usize,
    buf: &str,
    span: Option<Span>,
    e: &crate::sprites::toml_utils::GeneralSchemaError,
) -> FmtRet {
    use crate::sprites::toml_utils::GeneralSchemaError::*;
    use crate::sprites::toml_utils::VersionError::*;
    use log::Level::*;
    match e {
        Version(Missing) =>
            message_with_evidence(f, Error, file, 0, "", None,
                format_args!("version field is missing")
            ),
        Version(NonString) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("version field must be a string")
            ),
        Version(Unrecognizable) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("unrecognizable version value")
            ),
        Version(NotCompatible) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("mythic version is not compatible with current version {}.{}", MYTHIC_VERSION.major, MYTHIC_VERSION.minor)
            ),
        UnrecognizedGlobalField =>
            message_with_evidence(f, Warn, file, lineno, buf, span,
                format_args!("ignoring unrecognized global field")
            ),
        UnrecognizedField =>
            message_with_evidence(f, Warn, file, lineno, buf, span,
                format_args!("ignoring unrecognized field")
            ),
        Retrieve(toml::RetrieveError::FieldNotFound(field_name)) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("required field '{field_name}' is missing")
            ),
        Retrieve(toml::RetrieveError::IncompatibleType(expected, found)) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("this field should be '{expected}' but found '{found}'")
            ),
        Retrieve(toml::RetrieveError::Negative) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("this field should be non-negative number")
            ),
        PathEmpty =>
            message_with_evidence(f, Warn, file, lineno, buf, span,
                format_args!("path string cannot be empty")
            ),
        PathAbsolute =>
            message_with_evidence(f, Warn, file, lineno, buf, span,
                format_args!("path must be relative to containing file's path")
            ),
    }
}

fn sprite_path_error_message_with_evidence(
    f: &mut std::fmt::Formatter,
    file: &str,
    lineno: usize,
    buf: &str,
    span: Option<Span>,
    e: &crate::sprites::decls::SpritePathError
) -> FmtRet {
    use crate::sprites::decls::SpritePathError::*;
    use log::Level::*;
    match e {
        CannotHandlePath =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("sprite path must refer to a toml file or a directory containing one")
            ),
        CannotReadDir =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("sprite path refers to a directory but cannot read it")
            ),
        MultipleTomlInPath =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("sprite path refers to a directory but there are many toml files to select from")
            ),
        NoTomlInPath =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("sprite path refers to a directory but there is no toml file to select")
            ),
        NoSuchPath =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("sprite path refers to nothing in the file system")
            ),
        IOError(e) =>
            message_with_evidence(f, Error, file, lineno, buf, span,
                format_args!("could not load sprite toml file: {e}")
            ),
    }
}

impl<'src> Display for crate::sprites::decls::SpriteDeclLoadReport<'src> {
    fn fmt(&self, f: &mut Formatter) -> FmtRet {
        use log::Level::*;
        let file = self.file.as_rel().to_string_lossy();
        let file = file.as_ref();
        let (buf, span) = lineview(self.src, self.pos.span);
        let span = Some(span);

        use crate::sprites::decls::SpriteDeclLoadReportKind::*;

        match &self.kind {
            General(e) => general_schema_error_message_with_evidence(f, file, self.pos.line, buf, span, e),

            IOError(e) =>
                message_with_evidence(f, Error, file, 0, "", None,
                    format_args!("could not read; {e}")
                ),

            TomlParseError(parse_error) =>
                parse_error.message_with_evidence(f, file, self.pos.line, buf, span),

            SpritePath(e) => if self.src.is_empty() {
                // if from Sprites::reload_sprite, it does not have list.toml loaded => no evidence
                sprite_path_error_message_with_evidence(f, file, self.pos.line, "", None, e)
            } else {
                sprite_path_error_message_with_evidence(f, file, self.pos.line, buf, span, e)
            }
        }
    }
}


impl<'src, 'a> Display for crate::sprites::controller::ControllerLoadReport<'src> {
    fn fmt(&self, f: &mut Formatter) -> FmtRet {
        use log::Level::*;
        use crate::sprites::{clip, controller::{ConditionError, ControllerLoadReportKind, SchemaError, UnknownIdentifierError}};
        use crate::parser::{expr, Span};
        let (buf, span_) = lineview(self.src, self.pos.span);
        let span = Some(span_);
        let file=  self.file.as_rel().to_string_lossy();
        let file = file.as_ref();

        match &self.kind {
            ControllerLoadReportKind::ClipLoadError(clipload_error) => {
                use clip::ClipLoadError::*;
                match clipload_error {
                    CannotRead(e) =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span,
                            format_args!("while loading clip file: {e}")
                        ),
                    WebPAnimDecoderOptionsInit | WebPAnimDecoderNew | WebPAnimDecoderGetInfo | WebPAnimDecoderGetNext =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span,
                            // TODO use more elaborate error message
                            format_args!("while processing clip file: {clipload_error:?}")
                        ),
                }
            }

            ControllerLoadReportKind::TransConditionError(cond_error) => {
                use expr::LexError::*;
                use expr::ParseError::*;
                // use expr::SemanticError::*;
                use ConditionError::*;
                use UnknownIdentifierError::*;
                match cond_error {
                    SyntaxError(parse_error) => match &parse_error {
                        UnexpectedToken { found, expected, } =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span,
                                format_args!("unexpected {}, expecting {}", found.repr(), expected)
                            ),
                        Unrecognized(lex_error) => {
                            let msg = match lex_error {
                                UnexpectedChar => "unexpected character",
                                Newline => "newline found mid expression",
                                InvalidLiteral => "invalid literal",
                                NonAscii => "non ascii character",
                                MalformedIdentifierPath => {
                                    let s = &self.src[self.pos.span.start..self.pos.span.end];
                                    if s.chars().last() == Some('.') {
                                        "trailing period in identifier path"
                                    } else {
                                        "malformed identifier path"
                                    }
                                },
                            };
                            message_with_evidence(f, Error, file, self.pos.line, buf, span,
                                format_args!("{}", msg)
                            )
                        }
                    }

                    SemanticError(semantic_error) => match semantic_error {
                        expr::SemanticError::UnknownIdentifier(Parameter(cand)) => {
                            message_with_evidence(f, Error, file, self.pos.line, buf, span,
                                format_args!("this parameter is not recognized (subsequent errors for this parameter may be silenced)")
                            )?;
                            if let Some(cand) = cand {
                                // create fake source buffer
                                let path = app_paths().sprite_list();
                                let path = path.as_rel().to_string_lossy();
                                let buf = format!("param.{} = \"{}\"", cand.key, cand.val);
                                let span = Some(Span { start: 6, end: 6 + cand.key.len() });
                                message_with_evidence(f, Info, &path, cand.lineno, &buf, span,
                                    format_args!("Did you mean '{}'?", cand.key)
                                )?;
                            }
                            Ok(())
                        }
                        expr::SemanticError::UnknownIdentifier(IdentPath(uie)) => {
                            let ident_path = span_.slice(buf);
                            let parameterized = &uie.realpath != ident_path;

                            let ident_span = if uie.opaque.misc == 0 {
                                Span::whole(&uie.realpath)
                            } else if let Some(span) = Span::split(&uie.realpath, '.').nth(uie.opaque.misc as usize) {
                                span
                            } else {
                                Span::ending(&uie.realpath)
                            };

                            if ! parameterized {
                                let ident_span = span_.unframe(ident_span);
                                message_with_evidence(f, Error, file, self.pos.line, buf, Some(ident_span),
                                    format_args!("identifier path is not recognized by plugin '{}': {} (subsequent errors for this identifier path may be silenced)", uie.plugin, uie.opaque)
                                )
                            } else {
                                message_with_evidence(f, Error, "<condition expression>", 1, &uie.realpath, Some(ident_span),
                                    format_args!("identifier path is not recognized by plugin '{}': {} (subsequent errors for this identifier path may be silenced)", uie.plugin, uie.opaque)
                                )?;
                                message_with_evidence(f, Info, file, self.pos.line, buf, span,
                                    format_args!("instantiated from here")
                                )
                            }
                        }
                        expr::SemanticError::TypeMismatch { expected, found, } =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span,
                                format_args!("type mismatch. expected: {expected}, found: {found}")
                            ),
                    }

                    RetType(ty) =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span,
                            format_args!("condition expression must evaluate to boolean, found: {ty}")
                        ),
                }
            }

            ControllerLoadReportKind::TomlParseError(parse_error) => {
                parse_error.message_with_evidence(f, file, self.pos.line, buf, span)
            }

            ControllerLoadReportKind::SpriteSchemaError(scheme_error) => {
                use SchemaError::*;
                match scheme_error {
                    General(e) => general_schema_error_message_with_evidence(f, file, self.pos.line, buf, span, e),

                    UnknownDestState =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span,
                            format_args!("unknown transition destination, discarding the transition")
                        ),
                    NoAvailableClip =>
                        // message_with_evidence(f, Warn, file, self.pos.line, buf, span,
                        //     format_args!("state '{state_name}' hash no clips to select from, using empty clip")
                        message_with_evidence(f, Error, file, self.pos.line, buf, span,
                            format_args!("this state has no clips to select from")
                        ),
                    NoState =>
                        // message_with_evidence(f, Warn, file, self.pos.line, buf, span,
                        //     format_args!("state '{state_name}' hash no clips to select from, using empty clip")
                        message_with_evidence(f, Error, file, 0, "", None,
                            format_args!("the sprite has states defined, discarding sprite")
                        ),
                }
            }
        }
    }
}


impl<'a> std::fmt::Display for crate::worker::AppInitReport<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::worker::AppInitReportKind::*;
        use crate::parser;
        let (buf, span) = parser::lineview(self.src, self.pos.span);
        match &self.kind {
            ConfigParse(e) => {
                e.message_with_evidence(
                    f, &self.file.as_rel().to_string_lossy(), self.pos.line, buf, Some(span)
                )
            }

            MaxLogLevelType(e) => {
                parser::message_with_evidence(
                    f, log::Level::Error, &self.file.as_rel().to_string_lossy(),
                    self.pos.line, buf, Some(span),
                    format_args!("value for field 'max-log-level' should be '{}' but found '{}', using default (=warn)", e.expected, e.found)
                )
            }

            MaxLogLevelValue => {
                parser::message_with_evidence(
                    f, log::Level::Error, &self.file.as_rel().to_string_lossy(),
                    self.pos.line, buf, Some(span),
                    format_args!("value for field 'max-log-level' should be one of [off, error, warn, info, debug, trace], using default (=warn)")
                )
            }

            NumLogFiles(e) => {
                parser::message_with_evidence(
                    f, log::Level::Error, &self.file.as_rel().to_string_lossy(),
                    self.pos.line, buf, Some(span),
                    format_args!("value for field 'num-log-files' should be '{}' but found '{}', using default (=10)", e.expected, e.found)
                )
            }

            UnrecognizedField => {
                parser::message_with_evidence(
                    f, log::Level::Error, &self.file.as_rel().to_string_lossy(),
                    self.pos.line, buf, Some(span),
                    format_args!("unrecognized field")
                )
            }
        }
    }
}


impl<'a> Display for crate::sensing::OpaqueError<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use crate::sensing::OpaqueErrorMsgFail;
        match self.message {
            Ok(ref s) => write!(f, "[{}] {}", self.errcode, s),
            Err(OpaqueErrorMsgFail::Error(e)) => write!(f, "[{}] <ERR {e}>", self.errcode),
            Err(OpaqueErrorMsgFail::Null) => write!(f, "[{}] <NULL>", self.errcode),
            Err(OpaqueErrorMsgFail::NotUtf8(s)) =>
                write!(f, "[{}] {s}{}..<UTF8 ERR>", self.errcode, char::REPLACEMENT_CHARACTER),
        }
    }
}

impl<'a> Display for crate::sensing::OpaqueErrorOwned {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use crate::sensing::OpaqueErrorMsgFail;
        match self.message {
            Ok(ref s) => write!(f, "[{}] {}", self.errcode, s),
            Err(OpaqueErrorMsgFail::Error(e)) => write!(f, "[{}] <ERR {e}>", self.errcode),
            Err(OpaqueErrorMsgFail::Null) => write!(f, "[{}] <NULL>", self.errcode),
            Err(OpaqueErrorMsgFail::NotUtf8(s)) =>
                write!(f, "[{}] {s}{}..<UTF8 ERR>", self.errcode, char::REPLACEMENT_CHARACTER),
        }
    }
}