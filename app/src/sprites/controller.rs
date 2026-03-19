use crate::base::{AutoSize, AppPath, log_report, is_version_compatible, MYTHIC_VERSION};
use crate::parser::{expr, toml, Pos, WithSpan, WithPos, message_with_evidence, lineview};
use crate::sensing::{Sensors, SensingId};
use super::clip::{self, Clip, ClipId, ClipBank};
use super::decls::Params;

use std::path::Path;

/// Resolver

pub trait TypeResolver {
    fn resolve_type(&mut self, path: &str) -> Result<(expr::Type, SensingId), expr::SemanticError>;
}

pub trait ValueResolver {
    fn resolve_value(&self, sid: SensingId) -> Option<expr::Value>;
}

// wrapper to enable 'rollback' of registerations when error occurs
struct SensorsWrapper<'a> {
    inner: &'a mut Sensors,
    params: &'a Params,
    registered: Vec<SensingId>,
}

impl<'a> SensorsWrapper<'a> {
    pub fn new(inner: &'a mut Sensors, params: &'a Params) -> Self {
        Self { inner, params, registered: vec![] }
    }

    pub fn commit(mut self) {
        self.registered.clear();
    }
}

impl Drop for SensorsWrapper<'_> {
    fn drop(&mut self) {
        // automatic rollback unless committed
        for sid in &self.registered {
            self.inner.unregister(*sid);
        }
    }
}

impl TypeResolver for SensorsWrapper<'_> {
    fn resolve_type(&mut self, path: &str) -> Result<(expr::Type, SensingId), expr::SemanticError> {
        let path = path.split('.').map(|ident| {
            let Some(param) = path.strip_prefix('$') else { return Ok(ident) };
            self.params.lookup(param).ok_or(expr::SemanticError::UnknownParam(ident.to_string()))
        }).collect::<Result<String, _>>()?;

        if let Some((t, sid)) = self.inner.register(&path) {
            self.registered.push(sid);
            Ok((t, sid))
        } else {
            Err(expr::SemanticError::UnknownIdentifier(path))
        }
    }
}

impl<'a> ValueResolver for Sensors {
    fn resolve_value(&self, sid: SensingId) -> Option<expr::Value> {
        self.read(sid)
    }
}


/// ConditionExpr

struct ConditionExpr {
    inner: Option<ConditionExprInner>
}
struct ConditionExprInner {
    arena: expr::Arena<expr::Expr>,
    root: expr::ExprId,
}

impl std::fmt::Debug for ConditionExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Some(inner) = &self.inner {
            expr::pretty_print(f, "", &inner.arena, Some(inner.root))
        } else {
            f.write_str("<empty>")
        }
    }
}

enum ConditionError {
    SyntaxError(expr::ParseError),
    SemanticError(expr::SemanticError),
    RetType(expr::Type),
}

impl From<WithSpan<expr::ParseError>> for WithSpan<ConditionError> {
    fn from(value: WithSpan<expr::ParseError>) -> Self {
        value.map(ConditionError::SyntaxError)
    }
}

impl From<WithSpan<expr::SemanticError>> for WithSpan<ConditionError> {
    fn from(value: WithSpan<expr::SemanticError>) -> Self {
        value.map(ConditionError::SemanticError)
    }
}

impl ConditionExpr {
    pub fn load(src: &str, sensors: &mut Sensors, params: &Params) -> Result<Self, WithSpan<ConditionError>> {
        let (mut arena_expr, arena_span, expr) = expr::Parser::new(src).parse()?;

        let Some(expr) = expr else {
            return Ok(ConditionExpr { inner: None });
        };

        let mut sw = SensorsWrapper::new(sensors, params);

        let ty = expr::semantic_pass(src, &mut arena_expr, &arena_span, expr, &mut sw)?;

        if ty != expr::Type::Bool {
            return Err(WithSpan::nil(ConditionError::RetType(ty)));
        }

        sw.commit(); // prevent sensor id registeration rollback

        Ok(ConditionExpr { inner: Some(ConditionExprInner { arena: arena_expr, root: expr }) })
    }

    pub fn unload(self, sensors: &mut Sensors) {
        let Some(inner) = self.inner else { return };
        expr::unregister(&inner.arena, inner.root, &mut |sid| sensors.unregister(sid));
    }

    pub fn eval(&self, resolver: &impl ValueResolver) -> bool {
        if let Some(inner) = &self.inner {
            unsafe { expr::eval_expr(&inner.arena, inner.root, resolver).bool }
        } else {
            true
        }
    }
}


/// Controller

enum SpriteSchemaError<'a> {
    VersionMissing,
    VersionNotString,
    VersionUnrecognizable,
    VersionNotCompatible, // current version

    UnrecognizedGlobalField,
    UnrecognizedField,

    UnknownDestState,
    NonStringCondition(&'static str), // found type

    ClipWeightNotNumber(&'static str), // found type
    ClipWeightNegative,
    NotAllowedClipType,
    ClipPathMissing,
    ClipPathNotString,
    ClipValueEmpty,
    ClipValueAbsolute,
    NoAvailableClip(&'a str), // state name
}

enum ControllerLoadReportKind<'src> {
    ClipLoadError(clip::ClipLoadError),
    ConditionError(ConditionError),
    TomlParseError(toml::ParseError<'src>),
    SchemaError(SpriteSchemaError<'src>),
}

struct ControllerLoadReport<'src> {
    file: &'src AppPath,
    src: &'src str, // must refer to the whole file
    pos: Pos, // must refer to file-level offsets
    kind: ControllerLoadReportKind<'src>,
}

impl<'src> ControllerLoadReport<'src> {
    /// expr module's errors are reported with Pos's that are framed with the string value.
    /// This will translate the Pos to file-level.
    fn from_cond(file: &'src AppPath, src: &'src str, expr_pos: Pos, cond_e: WithSpan<ConditionError>) -> Self {
        let pos = Pos {
            span: cond_e.span + expr_pos.span.start + 1, // +1 for leading double quote
            ..expr_pos
        };
        Self { file, src, pos, kind: ControllerLoadReportKind::ConditionError(cond_e.val) }
    }
}

use std::fmt::{Formatter, Display};
type FmtRet = std::fmt::Result;

impl<'src> Display for ControllerLoadReport<'src> {
    fn fmt(&self, f: &mut Formatter) -> FmtRet {
        use log::Level::*;
        let (buf, span) = lineview(self.src, self.pos.span);
        let span = Some(span);
        let file=  self.file.as_rel().to_string_lossy();
        let file = file.as_ref();

        match &self.kind {
            ControllerLoadReportKind::ClipLoadError(clipload_error) => {
                use clip::ClipLoadError::*;
                match clipload_error {
                    CannotRead(e) =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "while loading clip file: {e}")
                        ),
                    WebPAnimDecoderNew | WebPAnimDecoderGetInfo | WebPAnimDecoderGetNext =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            // TODO use more elaborate error message
                            write!(f, "while processing clip file: {clipload_error:?}")
                        ),
                }
            }

            ControllerLoadReportKind::ConditionError(cond_error) => {
                use expr::LexError::*;
                use expr::ParseError::*;
                use expr::SemanticError::*;
                use ConditionError::*;
                match cond_error {
                    SyntaxError(parse_error) => match &parse_error {
                        UnexpectedToken { found, expected, } =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                                write!(f, "unexpected {}, expecting {}", found.repr(), expected)
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
                            message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                                write!(f, "{}", msg)
                            )
                        }
                    }

                    SemanticError(semantic_error) => match semantic_error {
                        UnknownParam(param) =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                                write!(f, "parameter '{param}' is not recognized")
                                // TODO add Did you mean?
                            ),
                        UnknownIdentifier(path) =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                                write!(f, "identifier path '{path}' is not recognized")
                                // TODO add Did you mean?
                            ),
                        TypeMismatch { expected, found, } =>
                            message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                                write!(f, "type mismatch. expected: {expected}, found: {found}")
                            ),
                    }
                    RetType(ty) =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "condition expression must evaluate to boolean, found: {ty}")
                        ),
                }
            }

            ControllerLoadReportKind::TomlParseError(parse_error) => {
                parse_error.message_with_evidence(f, file, self.pos.line, buf, span)
            }

            ControllerLoadReportKind::SchemaError(scheme_error) => {
                use SpriteSchemaError::*;
                match scheme_error {
                    VersionMissing =>
                        message_with_evidence(f, Error, file, 0, "", None, |f|
                            write!(f, "mythic version is missing")
                        ),
                    VersionNotString =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "mythic version is not a string")
                        ),
                    VersionUnrecognizable =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "mythic version is unrecognizable")
                        ),
                    VersionNotCompatible =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "mythic version is not compatible with current version {}.{}", MYTHIC_VERSION.major, MYTHIC_VERSION.minor)
                        ),
                    UnrecognizedGlobalField =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "ignoring unrecognized global field")
                        ),
                    UnrecognizedField =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "ignoring unrecognized field")
                        ),
                    UnknownDestState =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "unknown transition destination, discarding the transition")
                        ),
                    NonStringCondition(type_str) =>
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "transition condition must be string but found {type_str}, using constant 'false' instead.")
                        ),
                    ClipWeightNotNumber(found) =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip weight must be a number but found {found}, using 0 instead")
                        ),
                    ClipWeightNegative =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip weight must be positive, using 0 instead")
                        ),
                    NotAllowedClipType =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip path value must be a string (relative path) or an inline table")
                        ),
                    ClipPathMissing =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip path is missing")
                        ),
                    ClipPathNotString =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip path should be a string")
                        ),
                    ClipValueEmpty =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip path string is empty")
                        ),
                    ClipValueAbsolute =>
                        message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                            write!(f, "clip path must be relative to containing file's path")
                        ),
                    NoAvailableClip(state_name) =>
                        // message_with_evidence(f, Warn, file, self.pos.line, buf, span, |f|
                        //     write!(f, "state '{state_name}' hash no clips to select from, using empty clip")
                        message_with_evidence(f, Error, file, self.pos.line, buf, span, |f|
                            write!(f, "state '{state_name}' hash no clips to select from, discarding sprite")
                        ),
                }
            }
        }
    }
}


/// StateMachine

#[derive(Debug, Clone, Copy)]
struct StateId(usize);

#[derive(Debug)]
struct Transition {
    dest: StateId,
    cond: ConditionExpr,
}

#[derive(Debug)]
struct State {
    name: String,
    clips: Vec<(ClipId, f32)>, // weight, clip
    trans: Vec<Transition>
}

#[derive(Debug)]
pub struct SpriteController {
    states: Vec<State>,
    current_state: StateId,
    clip_idx: usize,
    loop_counter: u32,
    frame_idx: usize,
    rand_state: u64,
}

impl SpriteController {
    fn rand(&mut self) -> f32 {
        // 2 param xor shift
        self.rand_state ^= self.rand_state << 7;
        self.rand_state ^= self.rand_state >> 9;
        (self.rand_state as f32) / (u64::MAX as f32)
    }

    // this method is too bulky, and does many things to load a sprite description. we may improve it by..
    //   using retrieve
    //   using extract
    //   returning WithPos<Kind> <- no, we need to ignore certain errors..
    //   not loading clip yet
    //   separating load_state method
    // but untill we have persuading reason that surpasses the refactor cost, will just keep it
    pub fn load(file: &AppPath, size: AutoSize, sensors: &mut Sensors, clipbank: &mut ClipBank, params: &Params) -> Option<Self> {
        use ControllerLoadReportKind::*;
        use SpriteSchemaError::*;

        let src = match std::fs::read_to_string(file) {
            Ok(src) => src,
            Err(e) => {
                log::error!("cannot read file {file}: {e}");
                return None;
            }
        };
        let src = src.as_str();

        let mut tbl = match toml::Parser::new(src).parse() {
            Ok(tbl) => tbl,
            Err(WithPos{ pos, val }) => {
                log_report(ControllerLoadReport { file, src, pos, kind: TomlParseError(val) });
                return None;
            }
        };

        { // version
            let Some(version) = tbl.pop("version") else {
                log_report(ControllerLoadReport { file, src, pos: Pos::nil(), kind: SchemaError(VersionMissing) });
                return None;
            };

            let pos = version.val.pos;

            let toml::Value::String(version) = version.val.val else {
                log_report(ControllerLoadReport { file, src, pos, kind: SchemaError(VersionNotString) });
                return None;
            };

            let Some(compat) = is_version_compatible(version) else {
                log_report(ControllerLoadReport { file, src, pos, kind: SchemaError(VersionUnrecognizable) });
                return None;
            };

            if ! compat {
                log_report(ControllerLoadReport { file, src, pos, kind: SchemaError(VersionNotCompatible) });
                return None;
            }
        }

        let (get_state_id, num_states) = {
            let mut names: Vec<&str> = vec![];
            for entry in tbl.0.iter() {
                if entry.key.val == "mythic_version" { continue; }

                match &entry.val.val {
                    toml::Value::Table(_v) => names.push(entry.key.val),
                    _ => {
                        log_report(ControllerLoadReport { file, src, pos: entry.key.pos, kind: SchemaError(UnrecognizedGlobalField) });
                        continue;
                    }
                }
            }

            let num_states = names.len();
            let gsi = move |state_name: &str| names.iter().position(|s| *s == state_name).map(StateId);

            (gsi, num_states)
        };

        let mut states = Vec::with_capacity(num_states);
        let state_slice = states.spare_capacity_mut();

        for entry in tbl.0.into_iter() {
            let Some(state_id) = get_state_id(entry.key.val) else { continue };
            let toml::Value::Table(mut table) = entry.val.val else { unreachable!() };

            fn validate_clip_path<'a>(path: WithPos<&'a str>, file: &AppPath, src: &str) -> Option<WithPos<&'a Path>> {
                let path: WithPos<&Path> = path.map(|s| s.trim().as_ref());
                if path.val == Path::new("") {
                    log_report(ControllerLoadReport { file, src, pos: path.pos, kind: SchemaError(ClipValueEmpty) });
                    None
                } else if path.val.is_absolute() {
                    log_report(ControllerLoadReport { file, src, pos: path.pos, kind: SchemaError(ClipValueAbsolute) });
                    None
                } else {
                    Some(path)
                }
            }

            let mut clip_infos: Vec<(WithPos<&Path>, f32, Option<u32>)> = vec![]; // path, weight, loop_count_override
            for clip_entry in table.pop_all("clip") {
                match clip_entry.val.val {
                    toml::Value::String(path) => {
                        if let Some(path) = validate_clip_path(clip_entry.val.pos.with(path), file, src) {
                            clip_infos.push((path, 1.0, None));
                        }
                    }
                    toml::Value::Table(mut clip) => {
                        let path = if let Some(path_entry) = clip.pop("path") {
                            let path = if let toml::Value::String(s) = path_entry.val.val { s } else {
                                log_report(ControllerLoadReport { file, src, pos: path_entry.val.pos, kind: SchemaError(ClipPathNotString) });
                                continue;
                            };
                            let Some(path) = validate_clip_path(path_entry.val.pos.with(path), file, src) else {
                                continue;
                            };
                            path
                        } else {
                            log_report(ControllerLoadReport { file, src, pos: clip_entry.val.pos, kind: SchemaError(ClipPathMissing) });
                            continue;
                        };

                        let weight = if let Some(weight_entry) = clip.pop("weight") {
                            let weight = if let toml::Value::Number(w) = weight_entry.val.val { w } else {
                                log_report(ControllerLoadReport { file, src, pos: weight_entry.val.pos, kind: SchemaError(ClipWeightNotNumber(weight_entry.val.val.type_str())) });
                                0.0
                            };
                            if weight < 0.0 {
                                log_report(ControllerLoadReport { file, src, pos: weight_entry.val.pos, kind: SchemaError(ClipWeightNegative) });
                                0.0
                            } else {
                                weight
                            }
                        } else {
                            1.0 // default weight value
                        };

                        let _ = clip.pop("loop_count");

                        for e in clip.0 {
                            log_report(ControllerLoadReport { file, src, pos: e.val.pos, kind: SchemaError(UnrecognizedField) });
                        }

                        clip_infos.push((path, weight, None));
                    }
                    _ => {
                        log_report(ControllerLoadReport { file, src, pos: entry.val.pos, kind: SchemaError(NotAllowedClipType) });
                    }
                }
            }

            let mut clips = vec![];
            for (clippath, weight, _) in clip_infos {
                let path = file.parent().unwrap().slash(clippath.val);
                match clipbank.load(&path, size) {
                    Ok(clipid) => clips.push((clipid, weight)),
                    Err(e) => {
                        log_report(ControllerLoadReport { file, src, pos: clippath.pos, kind: ClipLoadError(e) });
                        continue
                    }
                }
            }

            if clips.is_empty() {
                log_report(ControllerLoadReport { file, src, pos: entry.val.pos, kind: SchemaError(NoAvailableClip(entry.key.val)) });
                return None;
            }

            let mut trans = vec![];
            for entry in table.0 {
                let Some(dest) = get_state_id(entry.key.val) else {
                    log_report(ControllerLoadReport { file, src, pos: entry.key.pos, kind: SchemaError(UnknownDestState) });
                    continue
                };

                let cond = if let toml::Value::String(cond) = entry.val.val { cond } else {
                    log_report(ControllerLoadReport { file, src, pos: entry.val.pos, kind: SchemaError(NonStringCondition(entry.val.val.type_str())) });
                    "false"
                };

                let cond = match ConditionExpr::load(cond, sensors, params) {
                    Ok(cond) => cond,
                    Err(e) => {
                        log_report(ControllerLoadReport::from_cond(file, src, entry.val.pos, e));
                        continue;
                    }
                };

                trans.push(Transition { dest, cond, });
            }

            state_slice[state_id.0].write(State { name: entry.key.val.to_string(), clips, trans, });
        }

        unsafe { states.set_len(num_states); }

        let rand_state = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_nanos() as u64;

        let mut ret = Self { states, current_state: StateId(0), clip_idx: 0, loop_counter: 0, frame_idx: 0, rand_state, };
        ret.choose_clip();

        // activate transition-through for the first clip
        if ret.current_loop_count_max(clipbank) == 0 {
            ret.make_transition(sensors, clipbank);
        }
        Some(ret)
    }

    pub fn unload(mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) {
        for mut state in self.states.drain(..) {
            for tran in state.trans.drain(..) {
                tran.cond.unload(sensors);
            }

            for (clip, _weight) in state.clips.drain(..) {
                clipbank.unload(clip);
            }
        }
        // self.states.clear() // allow safe drop
    }

    pub fn get_frame<'c>(&self, clipbank: &'c ClipBank) -> Frame<'c>  {
        let clip_id = self.current_clip();
        let clip = clipbank.get(clip_id);
        Frame { clip, frame_idx: self.frame_idx, }
    }

    // returns true if state transition occured (even if state is not changed)
    // when clip's loop_count is 0, treat it as 1 instead
    // TODO make loop_count be overridable by toml script?
    pub fn advance(&mut self, sensors: &Sensors, clipbank: &ClipBank) -> bool {
        let state = &self.states[self.current_state.0];
        let clip_id = state.clips[self.clip_idx].0;
        let clip = clipbank.get(clip_id);

        // advance frame
        self.frame_idx += 1;
        if self.frame_idx < clip.len() { return false; }
        self.frame_idx = 0;

        self.loop_counter += 1;
        if clip.loop_count > 0 && self.loop_counter < clip.loop_count { return false; }
        self.loop_counter = 0;

        // passed the last frame, make transition
        self.make_transition(sensors, clipbank);
        return true;
    }

    fn make_transition(&mut self, sensors: &Sensors, clipbank: &ClipBank) {
        // make repeated transition as long as chosen clip has loop_count 0 and there are transition available; at most #states times.
        for _ in 0 .. self.states.len() {
            let state = &self.states[self.current_state.0];
            let Some(dest) = state.trans.iter().find(|tran| tran.cond.eval(sensors)).map(|tran| tran.dest) else {
                self.choose_clip();
                break;
            };

            self.current_state = dest;
            self.choose_clip();

            if self.current_loop_count_max(clipbank) > 0 {
                break;
            }
        }
        // Either current clip's loop count > 0, max transition-passing reached, no available transition.
        // For all cases we called choose_clip for current state, loop_counter, frame_idx are reset.
        // No further handleing needed.
    }

    fn choose_clip(&mut self) {
        let state = &self.states[self.current_state.0];
        let sum: f32 = state.clips.iter().map(|(_c,w)| w).sum();

        let choice = self.rand() * sum;

        let state = &self.states[self.current_state.0];
        self.clip_idx = state.clips.iter()
            .scan(0.0, |s, x| { *s += x.1; Some(*s) })
            .position(|s| s > choice)
            .unwrap_or(state.clips.len()-1);

        self.loop_counter = 0;
        self.frame_idx = 0;
    }

    pub fn current_clip(&self) -> ClipId {
        let state = &self.states[self.current_state.0];
        let clip_id = state.clips[self.clip_idx].0;
        clip_id
    }

    fn current_loop_count_max(&self, clipbank: &ClipBank) -> u32 {
        let state = &self.states[self.current_state.0];
        let clip_id = state.clips[self.clip_idx].0;
        let clip = clipbank.get(clip_id);
        clip.loop_count
    }
}

impl Drop for SpriteController {
    fn drop(&mut self) {
        if ! self.states.is_empty() { panic!("SpriteController must be manually 'unload'ed") }
    }
}

pub struct Frame<'clip> {
    clip: &'clip Clip,
    frame_idx: usize,
}

impl<'clip> Frame<'clip> {
    pub fn width (&self) -> usize    { self.clip.get_width() }
    pub fn height(&self) -> usize    { self.clip.get_height() }
    pub fn pixels(&self) -> &Vec<u8> { self.clip.get_pixels(self.frame_idx) }
    pub fn delay (&self) -> u32      { self.clip.get_delay(self.frame_idx) }
}