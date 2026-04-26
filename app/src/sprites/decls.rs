use crate::parser::{toml, Pos, WithPos};
use crate::base::{self, AppPath, log_user, app_paths, bounded_optimal_string_alignment_distance};
use crate::sensing::Sensors;
use super::{Controller, ClipBank, ClipLoadError, Frame, ClipId, controller::{CtrlEdit, PartialController, ControllerLoadReport}};
use super::toml_utils::*;

use std::path::Path;
use std::cell::RefCell;


/// Params

#[derive(Debug, Clone)]
pub struct ParamEntry {
    pub key: String,
    pub val: String,
    pub lineno: usize,
}

impl PartialEq for ParamEntry {
    fn eq(&self, other: &Self) -> bool {
        (&self.key, &self.val) == (&other.key, &other.val)
    }
}
impl Eq for ParamEntry {}

// keys in 'inner' are always in certain order, we can simply compare them for equality without lookups
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    // always sorted in reverse of lexical order of the parameter keys
    inner: Vec<ParamEntry>,
}

impl<'src> Params {
    pub fn new(mut raw: Vec<ParamEntry>) -> Self {
        // stable sort using param keys to collect duplicated keys in appearing order
        raw.sort_by(|ent1, ent2| ent1.key.cmp(&ent2.key));
        raw.reverse(); // dedup leaves first of the consecutives, we want the last.
        raw.dedup_by(|ent1,ent2| ent1.key == ent2.key);
        Self { inner: raw }
    }

    pub fn lookup(&self, name: &str) -> Option<&str> {
        self.inner.iter().rfind(|ent| ent.key == name).map(|ent| ent.val.as_str())
    }

    pub fn find_fuzzy_match(&self, needle: &str) -> Option<&ParamEntry> {
        let max_dist = if needle.len() < 5 {
            needle.len() / 2
        } else {
            needle.len() / 4
        };

        let mut buffer = vec![0; 3*(needle.len()+1)];

        let idx = self.inner.iter().enumerate().filter_map(|(i, hay)|
            bounded_optimal_string_alignment_distance(&hay.key, needle, max_dist, &mut buffer[..])
            .map(|d| (d, i))
        ).min().map(|(_, i)| i);

        idx.map(|i| &self.inner[i])
    }
}


/// Decls

pub enum SpritePathError {
    CannotHandlePath,
    CannotReadDir,
    MultipleTomlInPath,
    NoTomlInPath,
    NoSuchPath,
    IOError(std::io::Error),
}

pub enum SpriteDeclLoadReportKind {
    General(GeneralSchemaError),
    IOError(std::io::Error),
    TomlParseError(toml::ParseError),
    SpritePath(SpritePathError),
}

impl From<toml::ParseError> for SpriteDeclLoadReportKind {
    fn from(value: toml::ParseError) -> Self {
        SpriteDeclLoadReportKind::TomlParseError(value)
    }
}

impl From<GeneralSchemaError> for SpriteDeclLoadReportKind {
    fn from(value: GeneralSchemaError) -> Self {
        SpriteDeclLoadReportKind::General(value)
    }
}

impl From<SpritePathError> for SpriteDeclLoadReportKind {
    fn from(value: SpritePathError) -> Self {
        SpriteDeclLoadReportKind::SpritePath(value)
    }
}

impl From<toml::RetrieveError> for SpriteDeclLoadReportKind {
    fn from(value: toml::RetrieveError) -> Self {
        GeneralSchemaError::Retrieve(value).into()
    }
}

pub struct SpriteDeclLoadReport<'src> {
    pub file: &'src AppPath,
    pub src: &'src str, // must refer to the whole file
    pub pos: Pos, // must refer to file-level offsets
    pub kind: SpriteDeclLoadReportKind,
}

// parsed from toml table. as-is.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteDecl {
    pub name: String,
    pos: (i32, i32),
    scale: f64,
    params: Params,
    path: WithPos<AppPath>,
}

struct DeclsLoader<'src, 'caller> {
    table: &'caller toml::Table<'src>,
    // clipbank: &'caller mut ClipBank,
    // sensors: &'caller mut Sensors,

    errors: RefCell<Vec<WithPos<SpriteDeclLoadReportKind>>>,
}

impl<'src, 'caller> LoaderHelper<'src, 'caller, SpriteDeclLoadReportKind> for DeclsLoader<'src, 'caller> {
    fn err<T: Into<SpriteDeclLoadReportKind>>(&self, e: WithPos<T>) {
        self.errors.borrow_mut().push(e.into())
    }

    fn to_app_path(&self, path: &Path) -> AppPath {
        app_paths().sprite(path)
    }
}

impl<'src, 'caller> DeclsLoader<'src, 'caller> {
    pub fn new(
        table: &'caller toml::Table<'src>,
        // clipbank: &'caller mut ClipBank,
        // sensors: &'caller mut Sensors,
    ) -> Self {
        Self {
            table, /*clipbank, sensors,*/
            errors: RefCell::new(vec![]),
        }
    }

    pub fn load(self) -> (Vec<SpriteDecl>, Vec<WithPos<SpriteDeclLoadReportKind>>) {
        let mut table = TomlTableAccessor::new(self.table);

        if let Err(e) = table.check_version() {
            self.err(e);
            return (vec![], self.errors.take());
        }

        let mut decls = vec![];
        for section_entry in &table.inner.0 {
            if section_entry.key.val == "version" { continue; } // TODO better way to handle this?
            let section = match section_entry.val.val.extract::<toml::Table>() {
                Ok(section) => section,
                Err(_e) => {
                    self.err(section_entry.key.pos.with(GeneralSchemaError::UnrecognizedGlobalField));
                    continue;
                }
            };

            let name = section_entry.key.val;
            let Some(decl) = self.load_decl(name, section_entry.val.pos.with(section)) else {
                continue
            };

            decls.push(decl);
        }

        (decls, self.errors.take())
    }

    fn load_decl(&self, name: &str, decl: WithPos<&toml::Table<'src>>) -> Option<SpriteDecl> {
        let WithPos { val: decl, pos: decl_pos } = decl;

        let mut table = TomlTableAccessor::new(decl);
        let mut unrecoverable = false;

        // required field
        let path = match table.retrieve::<&str>("sprite", Some(decl_pos)) {
            Ok(Some(WithPos { val: path, pos })) => match self.validate_path(path) {
                Ok(path) => pos.with(path),
                Err(e) => {
                    self.err(pos.with(e));
                    unrecoverable= true;
                    Pos::nil().with(app_paths().sprite("sprite.toml")) // placeholder
                }
            }
            Ok(None) => unreachable!(),
            Err(e) => {
                self.err(e);
                unrecoverable= true;
                Pos::nil().with(app_paths().sprite("sprite.toml")) // placeholder
            }
        };

        // optional fields
        let posx = self.get_default(&mut table, "pos.x", 0.0) as i32;
        let posy = self.get_default(&mut table,"pos.y", 0.0) as i32;
        let scale = self.get_default_noneg(&mut table,"scale", 1.0);

        // optional param fields
        let param_keys = table.inner.get_all_with_prefix("param.").map(|x| x.key.val).collect::<Vec<_>>();
        let mut params = vec![];
        for param_key in param_keys {
            let key = param_key.strip_prefix("param.").unwrap();

            // TODO support other value types
            match table.retrieve::<&str>(param_key, None) {
                Ok(Some(WithPos { val, pos })) => {
                    params.push(ParamEntry {
                        lineno: pos.line,
                        key: key.to_string(),
                        val: val.to_string(),
                    })
                }
                Ok(None) => unreachable!(),
                Err(e) => self.err(e),
            }
        }

        self.file_unrecognized(table);

        if ! unrecoverable {
            let params = Params::new(params);
            Some(SpriteDecl { scale, pos: (posx, posy), path, params, name: name.into() })
        } else { None }
    }

    // if the path is toml file, use it.
    // if the path is folder, look for sprite.toml in the folder.
    // if no sprite.toml, but only one toml, use it.
    // otherwise error.
    // clips are searched relatively to the sprite.toml file.
    fn load_controller(decl: &SpriteDecl, sensors: &mut Sensors, clipbank: &mut ClipBank) -> Result<Controller, Option<SpritePathError>> {
        let realpath = Self::find_real_sprite_path(&decl.path.val)?;
        Controller::load(&realpath, decl.scale, &decl.params, sensors, clipbank)
            .map_err(|e| e.map(SpritePathError::IOError))
    }

    unsafe fn load_controller_lazy(decl: &SpriteDecl, sensors: &mut Sensors) -> Result<(PartialController, String), Option<SpritePathError>> {
        let realpath = Self::find_real_sprite_path(&decl.path.val)?;
        Controller::load_lazy(&realpath, decl.scale, &decl.params, sensors)
            .map_err(|e| e.map(SpritePathError::IOError))
    }

    fn find_real_sprite_path<'a>(path: &'a AppPath) -> Result<std::borrow::Cow<'a, AppPath>, SpritePathError> {
        use SpritePathError::*;

        if ! path.exists() {
            // path does not exist
            Err(NoSuchPath)
        } else if path.is_file() {
            // path exists and it is a file
            Ok(std::borrow::Cow::Borrowed(path))
        } else if path.is_dir() {
            // path exists and it is a directory
            let Ok(files) = path.read_dir() else {
                return Err(CannotReadDir)
            };
            let path = path.join("sprite.toml");
            if path.is_file() {
                Ok(std::borrow::Cow::Owned(path))
            } else {
                let mut files = files
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|path| path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext == "toml")
                    .unwrap_or(false)
                );
                if let Some(path) = files.next() {
                    if files.next().is_none() {
                        Ok(std::borrow::Cow::Owned(AppPath::try_from(path).unwrap()))
                    } else { // multiple toml files in the dir
                        Err(MultipleTomlInPath)
                    }
                } else { // no toml file in the dir
                    Err(NoTomlInPath)
                }
            }
        } else {
            Err(CannotHandlePath)
        }
    }
}


/// Sprites

#[derive(Debug)]
pub enum SpriteEdit {
    Pos((i32, i32)),
    Ctrl(SpriteDecl, CtrlEdit),
}

#[derive(Debug)]
pub struct Sprites {
    decls: Vec<(SpriteDecl, Controller)>,
    decl_edits: Option<base::EditOps<(SpriteDecl,Controller), SpriteEdit>>,
}

impl Sprites {
    pub fn new() -> Self {
        Self { decls: vec![], decl_edits: None }
    }


    /// These methods are Sprites-Watcher interaface.
    /// They are mostly related with resource management.

    fn load_decls(path: &AppPath) -> (Vec<SpriteDecl>, String) {
        if ! path.exists() { return (vec![], String::new()) }

        let src_string = match std::fs::read_to_string(path) {
            Ok(src) => src,
            Err(e) => {
                log_user!("{}", SpriteDeclLoadReport {
                    file: path, src: "", pos: Pos::nil(), kind: SpriteDeclLoadReportKind::IOError(e),
                });
                return (vec![], String::new());
            }
        };
        let src = src_string.as_str();

        let report_error = |e: WithPos<SpriteDeclLoadReportKind>| {
            log_user!("{}", SpriteDeclLoadReport { file: path, src, pos: e.pos, kind: e.val });
        };

        let table = match toml::Parser::new(src).parse() {
            Ok(tbl) => tbl,
            Err(e) => {
                report_error(e.into());
                return (vec![], src_string);
            }
        };

        let loader = DeclsLoader::new(&table);
        let (decls, errors) = loader.load();

        for e in errors {
            report_error(e);
        }

        (decls, src_string)
    }

    unsafe fn gen_decl_edit(&self, orig_decl_idx: usize, new_decl: SpriteDecl, declpath: &AppPath, declsrc: &str, sensors: &mut Sensors, clipbank: &mut ClipBank) -> base::EditOp<(SpriteDecl, Controller), SpriteEdit> {
        use base::EditOp::*;
        match DeclsLoader::load_controller_lazy(&new_decl, sensors) {
            Err(error) => {
                if let Some(e) = error {
                    log_user!("{}", SpriteDeclLoadReport {
                        file: declpath, src: declsrc, pos: new_decl.path.pos, kind: e.into(),
                    });
                }
                KeepIt(orig_decl_idx)
            }
            Ok((pctrl, src)) => unsafe {
                let ctrl = &self.decls[orig_decl_idx].1;
                let (edits, errors) = ctrl.gen_edits(pctrl, clipbank);
                for e in errors {
                    log_user!("{}", ControllerLoadReport { file: &new_decl.path.val, src: &src, pos: e.pos, kind: e.val });
                }
                Update(orig_decl_idx, SpriteEdit::Ctrl(new_decl, edits))
            }
        }
    }

    // return false if there were unprocessed pending updates
    // true if the update has successfully queued
    pub fn reload(&mut self, path: &AppPath, sensors: &mut Sensors, clipbank: &mut ClipBank) -> bool {
        if self.decl_edits.is_some() { return false; }

        let (decls, src) = Self::load_decls(path);
        let src = src.as_str();

        let cost = |(sp1, _): &(SpriteDecl,Controller), sp2: &SpriteDecl| -> u32 {
            // let sp1 = match op {
            //     base::EditOp::KeepIt(idx) => self.decls[*idx].0.clone(),
            //     base::EditOp::Update(idx, SpriteEdit::Ctrl(sp,_)) => *sp,
            //     base::EditOp::Update(idx, SpriteEdit::Pos(pos)) => {
            //         let sp = self.decls[*idx].0.clone();
            //         sp.xpos = pos.0;
            //         sp.ypos = pos.1;
            //         sp
            //     }
            //     base::EditOp::Insert(idx, (sp, _)) => sp.clone(),
            //     base::EditOp::Remove(usize) => return u32::MAX,
            // };

            0

            + if sp1.path != sp2.path || sp1.scale != sp2.scale {
                50 } else { 0 } // can not reuse the window buffer, and possibly clips

            + if sp1.path != sp2.path {
                30 } else { 0 } // can not reuse sprite controller

            + if sp1.params != sp2.params || sp1.scale != sp2.scale {
                20 } else { 0 } // have to adjust sprite controller

            + if sp1 != sp2 {
                10 } else { 0 } // return 0 only if exactly matches
        }; // max cost is 110 < 200

        use base::EditOp::*;
        // should not remove editops entry
        let updates = base::generate_edit_script(&self.decls, decls, cost).0.into_iter().map(|op| match op {
            KeepIt(idx) => KeepIt(idx),
            Remove(idx) => Remove(idx),
            Insert(idx, new_decl) =>
                match DeclsLoader::load_controller(&new_decl, sensors, clipbank) {
                    Err(error) => {
                        if let Some(e) = error {
                            log_user!("{}", SpriteDeclLoadReport {
                                file: path, src: src, pos: new_decl.path.pos, kind: e.into(),
                            });
                        }
                        KeepIt(idx)
                    }
                    Ok(new_scon) => Insert(idx, (new_decl, new_scon)),
                }

            Update(idx, new_decl) if self.decls[idx].0.path == new_decl.path && self.decls[idx].0.scale == new_decl.scale && self.decls[idx].0.params == new_decl.params =>
                // only position changes
                Update(idx, SpriteEdit::Pos(new_decl.pos)),

            Update(idx, new_decl) => unsafe {
                self.gen_decl_edit(idx, new_decl, path, src, sensors, clipbank)
            }
        }).collect();

        self.decl_edits = Some(base::EditOps(updates));
        true
    }

    // return false if there were unprocessed pending updates
    // true if the update has successfully queued
    pub fn reload_sprite(&mut self, sprite_path: &AppPath, sensors: &mut Sensors, clipbank: &mut ClipBank) -> bool {
        if self.decl_edits.is_some() { return false; }

        let path = app_paths().sprite_list();
        let path = &path;

        let mut updates = base::EditOps::no_changes(self.decls.len());
        for (idx, (decl, controller)) in self.decls.iter().enumerate() { unsafe {
            if controller.get_path() != sprite_path { continue; }
            log::debug!("reloading sprite decl '{}'", sprite_path);

            // declsrc is used to report SpritePathError
            // Display impl aware that src may be empty for SpritePathError.
            updates.0[idx] = self.gen_decl_edit(idx, decl.clone(), path, "", sensors, clipbank);
        }}
        // Note that updates will be KeepIt or Update. no Remove or Insert

        self.decl_edits = Some(updates);
        true
    }

    // for all sprites try reloading with the path. trying on sprite with different clip path is effectively no-op.
    // unlike reload and reload_sprite, this method does not queues decls update.
    // instead, clipbank replaces replaces clip data internally.
    pub fn reload_clip(&self, path: &AppPath, clipbank: &mut ClipBank) -> Result<bool, ClipLoadError> {
        let mut context = clipbank.reload_context(path);
        let mut success = false;

        for (_decl, controller) in &self.decls {
            if controller.reload_clip(&mut context)? {
                success = true;
            }
        }

        Ok(success)
    }


    /// These methods are Sprites-Animator interaface.

    pub fn get_current_frame<'a>(&self, sprite_idx: usize, clipbank: &'a ClipBank) -> Frame<'a> {
        let (decl, controller) = &self.decls[sprite_idx]; // TODO report panic to log
        controller.get_current_frame(decl.pos, clipbank)
    }

    pub fn calc_lazy_rescale<'a>(&self, sprite_idx: usize, clipbank: &'a mut ClipBank) {
        let (decl, controller) = &self.decls[sprite_idx]; // TODO report panic to log
        controller.calc_lazy_rescale(clipbank)
    }

    pub fn get_current_clipid(&self, sprite_idx: usize) -> Option<ClipId> {
        let (_decl, controller) = &self.decls[sprite_idx]; // TODO report panic to log
        controller.get_current_clipid()
    }

    pub fn get_bounding_size(&self, sprite_idx: usize, clipbank: &ClipBank) -> (usize, usize) {
        let (_decl, controller) = &self.decls[sprite_idx]; // TODO report panic to log
        controller.bounding_size(clipbank)
    }

    pub fn advance(&mut self, sprite_idx: usize, sensors:&Sensors, clipbank: &mut ClipBank) {
        let (_decl, controller) = &mut self.decls[sprite_idx]; // TODO report panic to log
        controller.advance(sensors, clipbank);
    }

    pub fn is_pending_update_present(&self) -> bool {
        self.decl_edits.is_some()
    }

    pub fn apply_sprite_updates(&mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) -> Option<Vec<Option<usize>>> {
        // the returned vector of EditOp has different semantics from
        let Some(edits) = self.decl_edits.take() else { return None };

        use crate::base::EditOp::*;
        let mut cursor = 0;
        let mut new_idx = vec![None; self.decls.len()];
        for op in edits.0 {
            match op {
                KeepIt(idx) => {
                    new_idx[idx] = Some(cursor);
                    cursor += 1;
                }
                Remove(idx) => {
                    new_idx[idx] = None;
                    self.decls.remove(cursor);
                }
                Insert(_, (decl, scon)) => {
                    self.decls.insert(cursor, (decl, scon));
                    cursor += 1;
                }
                Update(idx, SpriteEdit::Pos(pos)) => {
                    new_idx[idx] = Some(cursor);
                    self.decls[cursor].0.pos = pos;
                    cursor += 1;
                }
                Update(idx, SpriteEdit::Ctrl(decl, ctrl_edit)) => unsafe {
                    new_idx[idx] = Some(cursor);
                    self.decls[cursor].0 = decl;
                    self.decls[cursor].1.apply_edits(ctrl_edit, sensors, clipbank);
                    cursor += 1;
                    // TODO possibly reset from render_queue
                }
            }
        }

        Some(new_idx)
    }




    pub fn unload(&mut self, sensor: &mut Sensors, clipbank: &mut ClipBank) {
        log::debug!("Sprites unloaded, with {} decls", self.decls.len());
        for (_, controller) in self.decls.drain(..) {
            controller.unload(sensor, clipbank);
        }
    }

    pub fn len(&self) -> usize {
        self.decls.len()
    }

    // pub fn take_updates(&mut self) -> Option<base::EditOps<(SpriteDecl,Controller), SpriteEdit>> {
    //     std::mem::take(&mut self.decl_edits)
    // }


}

impl Drop for Sprites {
    fn drop(&mut self) {
        if ! self.decls.is_empty() { panic!("Sprites must be manually 'unload'ed"); }
    }
}