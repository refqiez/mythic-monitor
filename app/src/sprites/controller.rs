use crate::base::{self, AutoSize, AppPath, app_paths, log_user, is_version_compatible};
use crate::parser::{expr, toml::{self, ExtractError}, Pos, Span, WithSpan, WithPos};
use crate::sensing::{Sensors, SensingId, OpaqueError, OpaqueErrorOwned};
use super::clip::{self, ClipId, ClipBank, Frame, ClipLoadError, ClipDataReloadContext};
use super::{decls::{Params, ParamEntry}, toml_utils::*};

use std::path::Path;
use std::collections::HashSet;
use std::cell::RefCell;

/// Resolver


pub struct UnknownIdentPathError {
    pub realpath: String,
    pub plugin: String,
    pub opaque: OpaqueErrorOwned,
}

pub enum UnknownIdentifierError {
    IdentPath(UnknownIdentPathError),
    Parameter(Option<ParamEntry>),
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
}

impl<'a> SensorsWrapper<'a> {
    pub fn new(inner: &'a mut Sensors, params: &'a Params) -> Self {
        Self { inner: std::ptr::NonNull::from_mut(inner), params, registered: vec![] }
    }

    pub fn commit(mut self) {
        self.registered.clear();
    }
}

impl Drop for SensorsWrapper<'_> {
    fn drop(&mut self) {
        // automatic rollback unless committed
        for sid in &self.registered { unsafe {
            if let Err((name, err)) = self.inner.as_mut().unregister(*sid) {
                log::error!("during discard of sensorwrapper, from plugin '{}': {}", name, err);
            }
        }}
    }
}

impl<'a> expr::TypeResolver<SensingId, UnknownIdentifierError> for SensorsWrapper<'a> {
    fn resolve_type(&mut self, path: &str) -> Result<(expr::Type, SensingId), WithSpan<UnknownIdentifierError>> {
        let realpath = Span::split(path, '.').map(|span| {
            let ident = &path[span.start..span.end];
            let Some(param) = path.strip_prefix('$') else { return Ok(ident) };
            self.params.lookup(param).ok_or(
                span.with(UnknownIdentifierError::Parameter(self.params.find_fuzzy_match(param).cloned()))
            )
        }).collect::<Result<String, _>>()?;

        match unsafe { self.inner.as_mut().register(&realpath) } {
            Ok((t, sid)) => {
                self.registered.push(sid);
                Ok((t, sid))
            }
            Err((plugin, opaque)) => {
                let err = UnknownIdentPathError { realpath, plugin: plugin.to_string(), opaque: opaque.to_owned() };
                Err(Span::whole(path).with(UnknownIdentifierError::IdentPath(err)))
            }
        }
    }
}

impl<'a> expr::ValueResolver<SensingId> for Sensors {
    fn resolve_value(&self, sid: SensingId) -> expr::Value {
        // self.read(sid)
        let val = self.read(sid);
        log::trace!("reading value: {sid:?} = {}", unsafe { val.float } );
        val
    }
}


/// ConditionExpr

// need to write this wrapper for Debug impl overwrite for Option
struct ConditionExpr {
    inner: Option<ConditionExprInner>
}

struct ConditionExprInner {
    arena: expr::Arena<expr::Expr<SensingId>>,
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

pub enum ConditionError {
    SyntaxError(expr::ParseError),
    SemanticError(expr::SemanticError<UnknownIdentifierError>),
    RetType(expr::Type),
}

impl<'a> From<expr::ParseError> for ConditionError {
    fn from(value: expr::ParseError) -> Self {
        ConditionError::SyntaxError(value)
    }
}

impl<'a> From<expr::SemanticError<UnknownIdentifierError>> for ConditionError {
    fn from(value: expr::SemanticError<UnknownIdentifierError>) -> Self {
        ConditionError::SemanticError(value)
    }
}

impl ConditionExpr {
    pub fn load<'a>(src: &str, sensors: &'a mut Sensors, params: &'a Params) -> Result<Self, WithSpan<ConditionError>> {
        let (mut arena_expr, arena_span, expr) = expr::Parser::new(src).parse().map_err(WithSpan::into)?;

        let Some(expr) = expr else {
            return Ok(ConditionExpr { inner: None });
        };

        let mut sw = SensorsWrapper::new(sensors, params);

        let ty: expr::Type = expr::semantic_pass(src, &mut arena_expr, &arena_span, expr, &mut sw).map_err(WithSpan::into)?;

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

    pub fn eval(&self, resolver: &impl expr::ValueResolver<SensingId>) -> bool {
        if let Some(inner) = &self.inner {
            unsafe { expr::eval_expr(&inner.arena, inner.root, resolver).bool }
        } else {
            true
        }
    }
}


/// Controller

pub enum SchemaError {
    General(GeneralSchemaError),

    NoAvailableClip,
    UnknownDestState,
    NoState,
}

pub enum ControllerLoadReportKind {
    ClipLoadError(clip::ClipLoadError),
    TransConditionError(ConditionError),
    TomlParseError(toml::ParseError),
    SpriteSchemaError(SchemaError),
}

pub struct ControllerLoadReport<'src> {
    pub file: &'src AppPath,
    pub src: &'src str, // must refer to the whole file
    pub pos: Pos, // must refer to file-level offsets
    pub kind: ControllerLoadReportKind,
}

impl ControllerLoadReportKind {
    /// expr module's errors are reported with Pos's that are framed within the string value.
    /// This will translate the Pos to file-level.
    fn from_cond(expr_pos: Pos, cond_e: WithSpan<ConditionError>) -> WithPos<Self> {
        let pos = Pos {
            span: cond_e.span + expr_pos.span.start + 1, // +1 for leading double quote
            ..expr_pos
        };
        pos.with(ControllerLoadReportKind::TransConditionError(cond_e.val))
    }
}
impl From<clip::ClipLoadError> for ControllerLoadReportKind {
    fn from(value: clip::ClipLoadError) -> Self {
        Self::ClipLoadError(value)
    }
}

impl From<SchemaError> for ControllerLoadReportKind {
    fn from(value: SchemaError) -> Self {
        Self::SpriteSchemaError(value)
    }
}

impl From<toml::ParseError> for ControllerLoadReportKind {
    fn from(value: toml::ParseError) -> Self {
        Self::TomlParseError(value)
    }
}

impl From<GeneralSchemaError> for ControllerLoadReportKind {
    fn from(value: GeneralSchemaError) -> Self {
        SchemaError::General(value).into()
    }
}

impl From<toml::RetrieveError> for ControllerLoadReportKind {
    fn from(value: toml::RetrieveError) -> Self {
        GeneralSchemaError::Retrieve(value).into()
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StateId(usize);

#[derive(Debug)]
pub struct Transition {
    dest: StateId,
    cond: ConditionExpr,
}

#[derive(Debug)]
struct ClipInfo {
    path: WithPos<AppPath>,
    weight: f64,
    offsetx: i32,
    offsety: i32,
    size: AutoSize,
    loop_count: Option<u32>,
    lazy_decode: Option<bool>,
    lazy_rescale: Option<bool>,
}

impl ClipInfo {
    fn same(&self, other: &Self) -> bool {
        self.path.val == other.path.val &&
        self.weight == other.weight &&
        self.offsetx == other.offsetx &&
        self.offsety == other.offsety &&
        self.size == other.size &&
        self.loop_count == other.loop_count &&
        self.lazy_decode == other.lazy_decode &&
        self.lazy_rescale == other.lazy_rescale
    }
}

#[derive(Debug)]
pub struct State {
    name: WithPos<String>,
    clips: Vec<(ClipInfo, ClipId)>,
    trans: Vec<Transition>
}

impl Drop for State {
    fn drop(&mut self) {
        if ! self.clips.is_empty() || ! self.trans.is_empty() { panic!("State must be manually 'unload'ed") }
    }
}

impl State {
    pub fn unload(mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) {
        for tran in self.trans.drain(..) {
            tran.cond.unload(sensors);
        }

        for (_clipinfo, clipid) in self.clips.drain(..) {
            clipbank.unload_clip(clipid);
        }
    }
}

#[derive(Debug)]
pub struct Controller {
    path: AppPath,
    states: Vec<State>,
    current_state: StateId,
    clip_idx: usize,
    loop_counter: u32,
    // frame_idx: usize,
    rand_state: u64,
}

// Controller but clips are not loaded
#[derive(Debug)]
pub struct PartialController(Controller);

#[derive(Debug)]
pub enum StateEdit {
    TransOnly(Vec<Transition>),
    Whole(State),
}

#[derive(Debug)]
pub enum CtrlEdit {
    Partial(base::EditOps<State,StateEdit>), // same path, partial update
    Whole(Controller), // path differs
}

struct StatesLoader<'src, 'caller> {
    table: &'caller toml::Table<'src>,
    clips_dir: &'caller AppPath,
    scale: f64,
    params: &'caller Params,
    sensors: &'caller mut Sensors,

    errors: RefCell<Vec<WithPos<ControllerLoadReportKind>>>,
}

impl ClipInfo {
    fn load_clip(&self, clipbank: &mut ClipBank) -> Result<ClipId, ClipLoadError> {
        let max_decode_frames = if self.lazy_decode.unwrap_or(true) { 2 } else { 0 }; // hardcoded frame buffer count
        clipbank.load_clip(&self.path.val, self.size, max_decode_frames, self.lazy_rescale, self.loop_count)
    }
}

impl<'src, 'caller> LoaderHelper<'src, 'caller, ControllerLoadReportKind> for StatesLoader<'src, 'caller> {
    fn err<T: Into<ControllerLoadReportKind>>(&self, e: WithPos<T>) {
        self.errors.borrow_mut().push(e.into())
    }

    fn to_app_path(&self, path: &Path) -> AppPath {
        self.clips_dir.join(path)
    }
}

impl<'src, 'caller> StatesLoader<'src, 'caller> {
    pub fn new(
        table: &'caller toml::Table<'src>,
        clips_dir: &'caller AppPath,
        scale: f64,
        params: &'caller Params,
        sensors: &'caller mut Sensors,
    ) -> Self {
        Self {
            table, clips_dir, scale, params, sensors,
            errors: RefCell::new(vec![]),
        }
    }

    // will remove clips that cannot be loaded. then check for NoAvailableClip
    // returns if recoverable
    pub unsafe fn load_clips_state(state: &mut State, clipbank: &mut ClipBank, errors: &mut Vec<WithPos<ControllerLoadReportKind>>) {
        state.clips.retain_mut(|(clip_info, clipid)| match clip_info.load_clip(clipbank) {
            Ok(id) => { assert!(clipid.is_nil()); *clipid = id; true }
            Err(e) => {
                errors.push(clip_info.path.pos.with(e).into());
                false
            }
        });

        if state.clips.is_empty() {
            errors.push(state.name.pos.with(SchemaError::NoAvailableClip).into());
        }
    }

    // will remove clips that cannot be loaded. then check for NoAvailableClip
    // unsafe since it will not unload previous ClipIds and panic if it's not nil
    pub unsafe fn load_clips_controller(mut pctrl: PartialController, clipbank: &mut ClipBank) -> (Controller, Vec<WithPos<ControllerLoadReportKind>>) {
        let mut errors = vec![];

        for state in &mut pctrl.0.states {
            Self::load_clips_state(state, clipbank, &mut errors);
        }

        (pctrl.0, errors)
    }

    // does not load clips yet, uses ClipId::nil instead
    // the caller needs to assure there are available clips for each states after clip loading
    unsafe fn load_lazy(mut self) -> (Option<Vec<State>>, Vec<WithPos<ControllerLoadReportKind>>) {
        let mut unrecoverable = false;
        let mut table = TomlTableAccessor::new(self.table);

        if let Err(e) = table.check_version() {
            self.err(e);
            return (None, self.errors.take())
        }

        let state_names = self.scan_states();

        // load states
        let mut states = vec![];
        for (_idx, name) in state_names.iter().enumerate() {
            let WithPos { val: section, pos } = table.retrieve(name, None).unwrap().unwrap();

            let Some(state) = self.load_state(pos.with(name), section, &state_names) else {
                unrecoverable = true;
                continue;
            };

            states.push(state);
        }

        if states.is_empty() {
            self.err(Pos::nil().with(SchemaError::NoState));
            unrecoverable = true;
        }

        self.file_unrecognized(table);

        let ret = if ! unrecoverable {
            Some(states)
        } else {
            for mut state in states {
                for tran in state.trans.drain(..) {
                    tran.cond.unload(self.sensors);
                }
            }
            None
        };

        (ret, self.errors.take())
    }

    // unsafe since it uses ClipId::nil
    unsafe fn load_state(&mut self, name: WithPos<&str>, table: &toml::Table<'src>, state_names: &[&'src str]) -> Option<State> {
        // all the fiels are either "clip" or destination state name.
        // there's no unrecognized field for State section.
        // let mut table = TomlTableAccessor::new(table);

        // load clips
        let mut clips = vec![];
        for clip_entry in table.get_all("clip") {
            let clip_info = match &clip_entry.val.val {
                toml::Value::String(path) => match self.clip_from_path(clip_entry.val.pos.with(path)) {
                    Ok(clip_info) => clip_info,
                    Err(e) => {
                        self.err(clip_entry.val.pos.with(e));
                        continue;
                    }
                }
                toml::Value::Table(clip) => {
                    let Some(clip_info) = self.load_clipinfo(clip_entry.key.pos.with(&clip)) else { continue };
                    clip_info
                }
                v => {
                    self.err(clip_entry.val.pos.with(toml::RetrieveError::IncompatibleType("string or table", v.type_str())));
                    continue;
                }
            };

            clips.push((clip_info, ClipId::nil()));
        }

        // load transitions
        let mut trans = vec![];
        for entry in &table.0 {
            let dest_name = entry.key.val;
            if dest_name == "clip" { continue; }

            let Some(dest) = Self::get_state_id(dest_name, state_names) else {
                self.err(entry.key.pos.with(SchemaError::UnknownDestState));
                continue
            };

            let cond = if let toml::Value::String(cond) = entry.val.val { cond } else {
                self.err(entry.val.pos.with(toml::RetrieveError::IncompatibleType("string", entry.val.val.type_str())));
                "false"
            };

            let cond = match ConditionExpr::load(cond, self.sensors, self.params) {
                Ok(cond) => cond,
                Err(e) => {
                    self.err(ControllerLoadReportKind::from_cond(entry.val.pos, e));
                    continue
                }
            };

            trans.push(Transition { dest, cond, });
        }

        Some(State { name: name.map(Into::into), clips, trans, })
    }

    fn load_clipinfo(&self, clip: WithPos<&'caller toml::Table<'src>>) -> Option<ClipInfo> {
        let WithPos { val: clip, pos: clip_pos } = clip;

        let mut unrecoverable = false;
        let mut table = TomlTableAccessor::new(clip);

        // required field
        let path = match table.retrieve::<&str>("path", Some(clip_pos)) {
            Ok(Some(WithPos { val: path, pos })) => match self.validate_path(path) {
                Ok(path) => pos.with(path),
                Err(e) => {
                    self.err(pos.with(e));
                    unrecoverable = true;
                    Pos::nil().with(app_paths().sprite_list()) // placeholder
                }
            }
            Ok(None) => unreachable!(),
            Err(e) => {
                self.err(e);
                unrecoverable = true;
                Pos::nil().with(app_paths().sprite_list()) // placeholder
            }
        };

        // optional fields
        let weight = self.get_default_noneg(&mut table, "weight", 1.0);
        let offsetx = (self.get_default::<f64>(&mut table, "offset.x", 0.0) * self.scale) as i32;
        let offsety = (self.get_default::<f64>(&mut table, "offset.y", 0.0) * self.scale) as i32;
        let loop_count = self.get_optional_noneg(&mut table, "loop_count").map(|x| x as u32);
        let width = self.get_optional_noneg(&mut table, "size.width").map(|x| x as usize);
        let height = self.get_optional_noneg(&mut table, "size.height").map(|x| x as usize);
        let lazy_decode = self.get_optional(&mut table,"lazy_decode");
        let lazy_rescale = self.get_optional(&mut table, "lazy_rescale");

        self.file_unrecognized(table);

        if !unrecoverable {
            Some(ClipInfo {
            path, weight, offsetx, offsety, size: AutoSize::new(width, height, self.scale),
            loop_count, lazy_decode, lazy_rescale,
            })
        } else { None }
    }

    fn clip_from_path(&self, WithPos { val: path, pos }: WithPos<&str>) -> Result<ClipInfo, ControllerLoadReportKind> {
        let path = self.validate_path(path)?;

        Ok(ClipInfo {
            path: pos.with(path), weight: 1.0, loop_count: None, size: AutoSize::new(None, None, self.scale),
            offsetx: 0, offsety: 0, lazy_decode: None, lazy_rescale: None,
        })
    }

    fn scan_states(&mut self) -> Vec<&'src str> {
        let mut state_names = vec![];
        for entry in self.table.0.iter() {
            match &entry.val.val {
                toml::Value::Table(_v) => state_names.push(entry.key.val),
                _ => continue,
            }
        }
        state_names
    }

    fn get_state_id(name: &str, state_names: &[&'src str]) -> Option<StateId> {
        state_names.iter().position(|s| *s == name).map(StateId)
    }
}

impl Controller {

    /// These methods are used in Sprite-Watcher interface calls

    pub fn load(
        path: &AppPath,
        scale: f64,
        params: &Params,
        sensors: &mut Sensors,
        clipbank: &mut ClipBank,
    ) -> Result<Self, Option<std::io::Error>> { unsafe {
        let (pctrl, src) = Self::load_lazy(path, scale, params, sensors)?;

        let (mut ctrl, errors) = StatesLoader::load_clips_controller(pctrl, clipbank);
        for e in errors {
            log_user!("{}", ControllerLoadReport { file: path, src: &src, pos: e.pos, kind: e.val });
        }

        // activate transition-through for the first clip
        if ctrl.current_loop_count_max(clipbank) == 0 {
            ctrl.make_transition(sensors, clipbank);
        }

        Ok(ctrl)
    }}

    // don't actually load clips, use ClipId for placeholder
    pub unsafe fn load_lazy(
        path: &AppPath,
        scale: f64,
        params: &Params,
        sensors: &mut Sensors,
    ) -> Result<(PartialController, String), Option<std::io::Error>> {
        let src_string = match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(e) => {
                return Err(Some(e));
            }
        };
        let src = src_string.as_str();

        let report_error = |e: WithPos<ControllerLoadReportKind>| {
            log_user!("{}", ControllerLoadReport { file: path, src, pos: e.pos, kind: e.val });
        };

        let table = match toml::Parser::new(src).parse() {
            Ok(tbl) => tbl,
            Err(e) => {
                report_error(e.into());
                return Err(None);
            }
        };

        let clips_dir = path.parent().unwrap(); // path points to a file. parent() cannot fail.
        let loader = StatesLoader::new(&table, &clips_dir, scale, params, sensors);
        let (states, errors) = loader.load_lazy();

        let mut unknown_param_seen: Vec<&str> = vec![];
        let mut unknown_ident_seen: Vec<&str> = vec![];
        for e in errors {
            if matches!(e.val, ControllerLoadReportKind::TransConditionError(ConditionError::SemanticError(expr::SemanticError::UnknownIdentifier(UnknownIdentifierError::Parameter(_))))) {
                let ident = e.pos.span.slice(src);
                if unknown_ident_seen.iter().find(|seen| **seen == ident).is_some() { continue; }
                unknown_ident_seen.push(ident);
            }

            if matches!(e.val, ControllerLoadReportKind::TransConditionError(ConditionError::SemanticError(expr::SemanticError::UnknownIdentifier(UnknownIdentifierError::IdentPath(_))))) {
                let param = e.pos.span.slice(src);
                if unknown_param_seen.iter().find(|seen| **seen == param).is_some() { continue; }
                unknown_param_seen.push(param);
            }

            report_error(e);
        }

        let Some(states) = states else {
            return Err(None);
        };

        let rand_state = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_nanos() as u64;

        let mut ret = Self { path: path.clone(), states, current_state: StateId(0), clip_idx: 0, loop_counter: 0, rand_state, };
        ret.choose_clip();

        Ok((PartialController(ret), src_string))
    }

    // try to preserve state_idx and clip_idx for seemless update of sprites
    // 'other' must be a lazily loaded Controller
    pub unsafe fn gen_edits(&self, mut pctrl: PartialController, clipbank: &mut ClipBank) -> (CtrlEdit, Vec<WithPos<ControllerLoadReportKind>>) {
        if pctrl.0.path != self.path {
            let (ctrl, errors) = StatesLoader::load_clips_controller(pctrl, clipbank);
            return (CtrlEdit::Whole(ctrl), errors)
        }

        fn cost(st1: &State, st2: &State) -> u32 {
            let mut c = 0;
            if st1.name.val != st2.name.val { c += 30; }
            c += 10 * st1.clips.iter().zip(st2.clips.iter())
            .filter(|(c1, c2)| c1.0.same(&c2.0)).count() as u32;
            c += 10 * st1.clips.iter().rev().zip(st2.clips.iter().rev())
            .filter(|(c1, c2)| c1.0.same(&c2.0)).count() as u32;
            c += 20 * st1.clips.len().abs_diff(st2.clips.len()) as u32;

            // it is hard to compare transition conditions, so we just assume they are different
            c += 10;

            c
        }

        // Generate edit script for current states -> new states.
        // For any updates or insert, we load clips for the new states (we don't find edit scrips for clips vector since there will be only handful of clips for a state)
        // If clip load fails, unload the state and replace the op with KeepIt
        let mut errors = vec![];
        let mut partial_states = vec![];
        std::mem::swap(&mut pctrl.0.states, &mut partial_states);
        use base::EditOp::*;
        let edits = base::generate_edit_script(&self.states, partial_states, cost).0.into_iter().map(|op| match op {
            KeepIt(_) => unreachable!(),
            Remove(idx) => Remove(idx),

            // Since we treat all transitions distinct, there will be no KeepIt entry.
            // - if there is any Insert or Remove, StateId s will change -> all the transitions must be updated
            // - KeepIt will drop new_state, preventing us from using it.
            // We have to manually find which of the Update pairs actually need Clip load.
            // We will replace transitions for all entries as we have already loaded them.
            Update(idx, mut st) => {
                let clips_unchanged = self.states[idx].clips.len() == st.clips.len() && self.states[idx].clips.iter().zip(st.clips.iter()).all(|(c1, c2)| c1.0.same(&c2.0));
                if clips_unchanged {
                    let (mut clips, mut trans) = (vec![], vec![]);
                    std::mem::swap(&mut clips, &mut st.clips); // prevent unloading nil ClipId
                    std::mem::swap(&mut trans, &mut st.trans);
                    Update(idx, StateEdit::TransOnly(trans))
                } else {
                    StatesLoader::load_clips_state(&mut st, clipbank, &mut errors);
                    Update(idx, StateEdit::Whole(st))
                }
            }

            Insert(idx, mut st) => {
                StatesLoader::load_clips_state(&mut st, clipbank, &mut errors);
                Insert(idx, st)
            }
        }).collect();

        (CtrlEdit::Partial(base::EditOps(edits)), errors)
    }

    pub fn reload_clip(&self, context: &mut ClipDataReloadContext) -> Result<bool, ClipLoadError> {
        let mut success = false;
        for state in &self.states {
            for (clip_info, clipid) in &state.clips {
                let max_decode_frames = if clip_info.lazy_decode.unwrap_or(true) { 2 } else { 0 }; // hardcoded frame buffer count
                if context.reload(*clipid, clip_info.size, max_decode_frames, clip_info.lazy_rescale, clip_info.loop_count)? {
                    success = true;
                }
            }
        }
        Ok(success)
    }


    /// These methods are used in Sprites-Animator interface calls

    pub fn get_current_frame<'a>(&self, outer_pos: (i32, i32), clipbank: &'a ClipBank) -> Frame<'a> {
        let state = &self.states[self.current_state.0];
        if state.clips.is_empty() { return Frame::empty(outer_pos) }
        let (clip_info, clipid) = &state.clips[self.clip_idx];
        let pos = (outer_pos.0 + clip_info.offsetx, outer_pos.1 + clip_info.offsety);
        clipbank.get_current_frame(*clipid, pos)
    }

    pub fn calc_lazy_rescale<'a>(&self, clipbank: &'a mut ClipBank) {
        let state = &self.states[self.current_state.0];
        if state.clips.is_empty() { return }
        let (clip_info, clipid) = &state.clips[self.clip_idx];
        clipbank.calc_lazy_rescale(*clipid)
    }

    pub fn get_current_clipid(&self) -> Option<ClipId> {
        let state = &self.states[self.current_state.0];
        state.clips.get(self.clip_idx).map(|x| x.1)
    }

    // returns clipid that has advanced
    pub fn advance(&mut self, sensors: &Sensors, clipbank: &mut ClipBank) {
        let state = &self.states[self.current_state.0];
        if state.clips.is_empty() {
            self.make_transition(sensors, clipbank);
            return;
        }

        let clipid = state.clips[self.clip_idx].1;

        // advance frame
        let rewinded = clipbank.advance(clipid);
        if ! rewinded { return; }

        let clip_loop_count = clipbank.get_loop_count_max(clipid);
        self.loop_counter += 1;
        if clip_loop_count > 0 && self.loop_counter < clip_loop_count { return; }
        self.loop_counter = 0;

        // passed the last frame, make transition
        self.make_transition(sensors, clipbank);
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
        if state.clips.is_empty() {
            self.clip_idx = 0;
            self.loop_counter = 0;
            return;
        }

        let sum: f64 = state.clips.iter().map(|(c,_)| c.weight).sum();

        let choice = self.rand() * sum;

        let state = &self.states[self.current_state.0];
        self.clip_idx = state.clips.iter()
            .scan(0.0, |s, x| { *s += x.0.weight; Some(*s) })
            .position(|s| s > choice)
            .unwrap_or(state.clips.len()-1);

        self.loop_counter = 0;
    }

    fn current_loop_count_max(&self, clipbank: &ClipBank) -> u32 {
        let state = &self.states[self.current_state.0];
        if state.clips.is_empty() { return 0 }
        let clipid = state.clips[self.clip_idx].1;
        clipbank.get_loop_count_max(clipid)
    }

    pub unsafe fn apply_edits(&mut self, edit: CtrlEdit, sensors: &mut Sensors, clipbank: &mut ClipBank) {
        match edit {
            CtrlEdit::Whole(ctrl) => {
                // self.unload()
                for state in self.states.drain(..) { state.unload(sensors, clipbank); }
                *self = ctrl;
            }
            CtrlEdit::Partial(edits) => {
                use base::EditOp::*;
                let mut cursor = 0;
                for op in edits.0 {
                    match op {
                        KeepIt(_idx) => unreachable!(),
                        Remove(idx) => {
                            if idx == self.current_state.0 {
                                self.reset_state_idx();
                            }
                            let ret = self.states.remove(cursor);
                            ret.unload(sensors, clipbank);
                        }
                        Insert(_idx, st) => {
                            self.states.insert(cursor, st);
                            cursor += 1;
                            // todo! invoke decode
                        }
                        Update(_idx, StateEdit::TransOnly(mut trans)) => {
                            std::mem::swap(&mut self.states[cursor].trans, &mut trans);
                            for tran in trans { tran.cond.unload(sensors); }
                            cursor += 1;
                        }
                        Update(idx, StateEdit::Whole(mut st)) => {
                            if idx == self.current_state.0 {
                                self.reset_state_idx();
                            }
                            std::mem::swap(&mut self.states[cursor], &mut st);
                            st.unload(sensors, clipbank);
                            cursor += 1;
                            // todo! invoke decode
                        }
                    }
                }
            }
        }
    }

    pub fn bounding_size(&self, clipbank: &ClipBank) -> (usize, usize) {
        let (err_width, err_height) = Frame::error((0,0)).size;
        let (loa_width, loa_height) = Frame::loading((0,0)).size;
        let mut max_width = err_width.max(loa_width);
        let mut max_height = err_height.max(loa_height);
        for state in &self.states {
            for (_, clipid) in &state.clips {
                let (width, height) = clipbank.get_size(*clipid);
                max_width = max_width.max(width);
                max_height = max_height.max(height);
            }
        }
        (max_width, max_height)
    }

    // misc utility methods

    pub fn unload(mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) {
        for state in self.states.drain(..) {
            state.unload(sensors, clipbank);
        }
    }

    fn rand(&mut self) -> f64 {
        // 2 param xor shift
        self.rand_state ^= self.rand_state << 7;
        self.rand_state ^= self.rand_state >> 9;
        (self.rand_state as f64) / (u64::MAX as f64)
    }

    pub fn get_path(&self) -> &AppPath {
        &self.path
    }


    pub fn reset_state_idx(&mut self) {
        self.current_state = StateId(0);
        self.clip_idx = 0;
        self.loop_counter = 0;
    }

    // pub fn current_clip(&self) -> ClipId {
    //     let state = &self.states[self.current_state.0];
    //     let clip_id = state.clips[self.clip_idx].0;
    //     clip_id
    // }


}

impl Drop for Controller {
    fn drop(&mut self) {
        if ! self.states.is_empty() { panic!("Controller must be manually 'unload'ed") }
    }
}