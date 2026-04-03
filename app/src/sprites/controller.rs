use crate::base::{AutoSize, AppPath, log_user, is_version_compatible};
use crate::parser::{expr, toml, Pos, Span, WithSpan, WithPos};
use crate::sensing::{Sensors, SensingId, OpaqueError};
use super::clip::{self, Clip, ClipId, ClipBank};
use super::decls::Params;

use std::path::Path;

/// Resolver

pub trait TypeResolver {
    fn resolve_type(&mut self, path: &str) -> Result<(expr::Type, SensingId), WithSpan<expr::SemanticError>>;
}

pub trait ValueResolver {
    fn resolve_value(&self, sid: SensingId) -> expr::Value;
}

pub struct UnknownIdentErr<'a> {
    pub realpath: String,
    pub plugin: &'a str,
    pub opaque: OpaqueError<'a>,
}

// wrapper to enable 'rollback' of registerations when error occurs
struct SensorsWrapper<'a> {
    // If we use *mut Sensors here, it will prevent us from storing errors in the same struct.
    // self.inner will always have lifetime of 'self so that self.inner.register returns OpaqueError<'self>
    // which conflicts with the definition.
    // This is safe since SensorsWrapper::new requires &'a mut Sensors and SensorsWrapper<'a> is forced to
    // be dropped before Sensors. The same goes for self.error.
    inner: std::ptr::NonNull<Sensors>,
    params: &'a Params,
    registered: Vec<SensingId>,
    // to hold last opaque error during resolve_type, since we don't want to give it to Expr
    error: Option<UnknownIdentErr<'a>>, // realpath, plugin, opaque
}

impl<'a> SensorsWrapper<'a> {
    pub fn new(inner: &'a mut Sensors, params: &'a Params) -> Self {
        Self { inner: std::ptr::NonNull::from_mut(inner), params, registered: vec![], error: None }
    }

    pub fn commit(mut self) {
        self.registered.clear();
    }
}

impl Drop for SensorsWrapper<'_> {
    fn drop(&mut self) {
        // automatic rollback unless committed
        for sid in &self.registered {
            unsafe { self.inner.as_mut().unregister(*sid); }
        }
    }
}

impl<'a> TypeResolver for SensorsWrapper<'a> {
    fn resolve_type(&mut self, path: &str) -> Result<(expr::Type, SensingId), WithSpan<expr::SemanticError>> {
        let realpath = Span::split(path, '.').map(|span| {
            let ident = &path[span.start..span.end];
            let Some(param) = path.strip_prefix('$') else { return Ok(ident) };
            self.params.lookup(param).ok_or(span.with(expr::SemanticError::UnknownParam))
        }).collect::<Result<String, _>>()?;

        match unsafe { self.inner.as_mut().register(&realpath) } {
            Ok((t, sid)) => {
                self.registered.push(sid);
                Ok((t, sid))
            }
            Err((plugin, opaque)) => {
                self.error = Some(UnknownIdentErr { realpath, plugin, opaque });
                Err(Span::whole(path).with(expr::SemanticError::UnknownIdentifier))
            }
        }
    }
}

impl<'a> ValueResolver for Sensors {
    fn resolve_value(&self, sid: SensingId) -> expr::Value {
        // self.read(sid)
        let val = self.read(sid);
        log::trace!("reading value: {sid:?} = {}", unsafe { val.float } );
        val
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

pub enum ConditionSemanticError<'a> {
    UnknownParam(&'a Params),
    UnknownIdentifier(UnknownIdentErr<'a>),
    TypeMismatch {
        expected: expr::Type,
        found: expr::Type,
    },
}

pub enum ConditionError<'a> {
    SyntaxError(expr::ParseError),
    SemanticError(ConditionSemanticError<'a>),
    RetType(expr::Type),
}

impl<'a> From<WithSpan<expr::ParseError>> for WithSpan<ConditionError<'a>> {
    fn from(value: WithSpan<expr::ParseError>) -> Self {
        value.map(ConditionError::SyntaxError)
    }
}


impl ConditionExpr {
    pub fn load<'a>(src: &str, sensors: &'a mut Sensors, params: &'a Params) -> Result<Self, WithSpan<ConditionError<'a>>> {
        let (mut arena_expr, arena_span, expr) = expr::Parser::new(src).parse()?;

        let Some(expr) = expr else {
            return Ok(ConditionExpr { inner: None });
        };

        let mut sw = SensorsWrapper::new(sensors, params);

        let ty: expr::Type = expr::semantic_pass(src, &mut arena_expr, &arena_span, expr, &mut sw)
            .map_err(|e| e.map(|kind| match kind {
                expr::SemanticError::UnknownParam => ConditionSemanticError::UnknownParam(params),
                expr::SemanticError::UnknownIdentifier => ConditionSemanticError::UnknownIdentifier(sw.error.take().unwrap()),
                expr::SemanticError::TypeMismatch { expected, found } =>
                    ConditionSemanticError::TypeMismatch { expected, found },
            }).map(ConditionError::SemanticError))?;

        if ty != expr::Type::Bool {
            // SensorsWrapper will roll back registerations automatically
            return Err(WithSpan::nil(ConditionError::RetType(ty)));
        }

        sw.commit(); // prevent sensor id registeration rollback

        Ok(ConditionExpr { inner: Some(ConditionExprInner { arena: arena_expr, root: expr }) })
    }

    pub fn unload(self, sensors: &mut Sensors) {
        let Some(inner) = self.inner else { return };
        expr::unregister(&inner.arena, inner.root, &mut |sid|
            // FIXME report errors up?
            if let Err((name, e)) = sensors.unregister(sid) {
                crate::worker::report_opaque_error(name, "unregister", e);
            }
        );
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

pub enum SpriteSchemaError {
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
    NoAvailableClip,
}

pub enum ControllerLoadReportKind<'a> {
    ClipLoadError(clip::ClipLoadError),
    TransConditionError(ConditionError<'a>),
    TomlParseError(toml::ParseError),
    SchemaError(SpriteSchemaError),
}

pub struct ControllerLoadReport<'src, 'a> {
    pub file: &'src AppPath,
    pub src: &'src str, // must refer to the whole file
    pub pos: Pos, // must refer to file-level offsets
    pub kind: ControllerLoadReportKind<'a>,
}

impl<'a> ControllerLoadReportKind<'a> {
    /// expr module's errors are reported with Pos's that are framed within the string value.
    /// This will translate the Pos to file-level.
    fn from_cond(expr_pos: Pos, cond_e: WithSpan<ConditionError<'a>>) -> WithPos<Self> {
        let pos = Pos {
            span: cond_e.span + expr_pos.span.start + 1, // +1 for leading double quote
            ..expr_pos
        };
        pos.with(ControllerLoadReportKind::TransConditionError(cond_e.val))
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
    clips: Vec<(ClipId, f64)>, // weight, clip
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
    fn rand(&mut self) -> f64 {
        // 2 param xor shift
        self.rand_state ^= self.rand_state << 7;
        self.rand_state ^= self.rand_state >> 9;
        (self.rand_state as f64) / (u64::MAX as f64)
    }

    // this method is too bulky, and does many things to load a sprite description. we may improve it by..
    //   using retrieve
    //   using extract
    //   returning WithPos<Kind> <- no, we need to ignore certain errors..
    //   not loading clip yet
    //   separating load_state method
    // but untill we have persuading reason that surpasses the refactor cost, will just keep it
    pub fn load(
        path: &AppPath,
        size: AutoSize,
        sensors: &mut Sensors,
        clipbank: &mut ClipBank,
        params: &Params,
    ) -> Result<Self, Option<std::io::Error>> {
        use ControllerLoadReportKind::*;
        use SpriteSchemaError::*;

        let src = match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(e) => {
                return Err(Some(e));
            }
        };
        let src = src.as_str();

        let report_error = |e: WithPos<ControllerLoadReportKind>| {
            log_user!("{}", ControllerLoadReport { file: path, src, pos: e.pos, kind: e.val });
        };

        let mut tbl = match toml::Parser::new(src).parse() {
            Ok(tbl) => tbl,
            Err(e) => {
                report_error(e.map(TomlParseError));
                return Err(None);
            }
        };

        { // version
            let Some(version) = tbl.pop("version") else {
                report_error(Pos::nil().with(SchemaError(VersionMissing)));
                return Err(None);
            };

            let pos = version.val.pos;

            let toml::Value::String(version) = version.val.val else {
                report_error(pos.with(SchemaError(VersionNotString)));
                return Err(None);
            };

            let Some(compat) = is_version_compatible(version) else {
                report_error(pos.with(SchemaError(VersionUnrecognizable)));
                return Err(None);
            };

            if ! compat {
                report_error(pos.with(SchemaError(VersionNotCompatible)));
                return Err(None);
            }
        }

        let (get_state_id, num_states) = {
            let mut names: Vec<&str> = vec![];
            for entry in tbl.0.iter() {
                if entry.key.val == "mythic_version" { continue; }

                match &entry.val.val {
                    toml::Value::Table(_v) => names.push(entry.key.val),
                    _ => {
                        report_error(entry.key.pos.with(SchemaError(UnrecognizedGlobalField)));
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

        let mut unknown_param_seen: Vec<&str> = vec![];
        let mut unknown_ident_seen: Vec<&str> = vec![];

        for entry in tbl.0.into_iter() {
            let Some(state_id) = get_state_id(entry.key.val) else { continue };
            let toml::Value::Table(mut table) = entry.val.val else { unreachable!() };

            fn validate_clip_path<'a>(path: WithPos<&'a str>) -> Result<WithPos<&'a Path>, WithPos<ControllerLoadReportKind<'static>>> {
                let path: WithPos<&Path> = path.map(|s| s.trim().as_ref());
                if path.val == Path::new("") {
                    Err(path.pos.with(SchemaError(ClipValueEmpty)))
                } else if path.val.is_absolute() {
                    Err(path.pos.with(SchemaError(ClipValueAbsolute)))
                } else {
                    Ok(path)
                }
            }

            let mut clip_infos: Vec<(WithPos<&Path>, f64, Option<f64>)> = vec![]; // path, weight, loop_count_override
            for clip_entry in table.pop_all("clip") {
                match clip_entry.val.val {
                    toml::Value::String(path) => {
                        match validate_clip_path(clip_entry.val.pos.with(path)) {
                            Ok(path) => clip_infos.push((path, 1.0, None)),
                            Err(e) => report_error(e),
                        }
                    }
                    toml::Value::Table(mut clip) => {
                        let path = if let Some(path_entry) = clip.pop("path") {
                            let path = if let toml::Value::String(s) = path_entry.val.val { s } else {
                                report_error(path_entry.val.pos.with(SchemaError(ClipPathNotString)));
                                continue;
                            };
                            match validate_clip_path(path_entry.val.pos.with(path)) {
                                Ok(path) => path,
                                Err(e) => { report_error(e); continue; }
                            }
                        } else {
                            report_error(clip_entry.val.pos.with(SchemaError(ClipPathMissing)));
                            continue;
                        };

                        let weight = if let Some(weight_entry) = clip.pop("weight") {
                            let weight = if let toml::Value::Number(w) = weight_entry.val.val { w } else {
                                report_error(weight_entry.val.pos.with(SchemaError(ClipWeightNotNumber(weight_entry.val.val.type_str()))));
                                0.0
                            };
                            if weight < 0.0 {
                                report_error(weight_entry.val.pos.with(SchemaError(ClipWeightNegative)));
                                0.0
                            } else {
                                weight
                            }
                        } else {
                            1.0 // default weight value
                        };

                        let _ = clip.pop("loop_count");

                        for e in clip.0 {
                            report_error(e.val.pos.with(SchemaError(UnrecognizedField)));
                        }

                        clip_infos.push((path, weight, None));
                    }
                    _ => {
                        report_error(entry.val.pos.with(SchemaError(NotAllowedClipType)));
                    }
                }
            }

            let mut clips = vec![];
            for (clippath, weight, _) in clip_infos {
                let path = path.parent().unwrap().slash(clippath.val);
                match clipbank.load(&path, size) {
                    Ok(clipid) => clips.push((clipid, weight)),
                    Err(e) => {
                        report_error(clippath.pos.with(ClipLoadError(e)));
                        continue
                    }
                }
            }

            if clips.is_empty() {
                report_error(entry.val.pos.with(SchemaError(NoAvailableClip)));
                return Err(None);
            }

            let mut trans = vec![];
            for entry in table.0 {
                let Some(dest) = get_state_id(entry.key.val) else {
                    report_error(entry.key.pos.with(SchemaError(UnknownDestState)));
                    continue
                };

                let cond = if let toml::Value::String(cond) = entry.val.val { cond } else {
                    report_error(entry.val.pos.with(SchemaError(NonStringCondition(entry.val.val.type_str()))));
                    "false"
                };

                let cond = match ConditionExpr::load(cond, sensors, params) {
                    Ok(cond) => cond,
                    Err(e) => {
                        if matches!(e.val, ConditionError::SemanticError(ConditionSemanticError::UnknownIdentifier(_))) {
                            let ident = e.span.slice(cond);
                            if unknown_ident_seen.iter().find(|seen| **seen == ident).is_some() { continue; }
                            unknown_ident_seen.push(ident);
                        }

                        if matches!(e.val, ConditionError::SemanticError(ConditionSemanticError::UnknownParam(_))) {
                            let param = e.span.slice(cond);
                            if unknown_param_seen.iter().find(|seen| **seen == param).is_some() { continue; }
                            unknown_param_seen.push(param);
                        }

                        report_error(ControllerLoadReportKind::from_cond(entry.val.pos, e));
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
        Ok(ret)
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
        let sum: f64 = state.clips.iter().map(|(_c,w)| w).sum();

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