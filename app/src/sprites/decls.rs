use crate::parser::{toml, Pos, WithPos, Span};
use crate::base::{Align, AppPath, AutoSize, log_user, app_paths, is_version_compatible, bounded_optimal_string_alignment_distance};
use crate::sprites::SpriteController;
use crate::sensing::Sensors;
use crate::sprites::clip::ClipBank;

use std::collections::HashMap;
use std::path::Path;

use crate::worker::watcher::{WindowUpdate, WindowUpdateKind};

#[derive(Debug)]
pub enum SpriteDeclLoadReportKind {
    IOError(std::io::Error),
    TomlParseError(toml::ParseError),

    VersionMissing,
    VersionNotString,
    VersionUnrecognizable,
    VersionNotCompatible,

    Retrieve(toml::RetrieveError<'static>),
    NoHorizontalPosition,
    NoVerticalPosition,
    SpritePathEmtpy,
    SpritePatyAbsolute,
    UnrecognizedField,

    CannotHandlePath,
    CannotReadDir,
    MultipleTomlInPath,
    NoTomlInPath,
    NoSuchPath,
    LoadIoError(Option<std::io::Error>),
    UnrecognizedEntry,

    NeedBothSize,
}

impl From<WithPos<toml::RetrieveError<'static>>> for WithPos<SpriteDeclLoadReportKind> {
    fn from(value: WithPos<toml::RetrieveError<'static>>) -> Self {
        value.map(SpriteDeclLoadReportKind::Retrieve)
    }
}

pub struct SpriteDeclLoadReport<'src> {
    pub file: &'src AppPath,
    pub src: &'src str, // must refer to the whole file
    pub pos: Pos, // must refer to file-level offsets
    pub kind: SpriteDeclLoadReportKind,
}


#[derive(Debug)]
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
#[derive(Debug, PartialEq, Eq)]
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

// parsed from toml table. as-is.
#[derive(Debug)]
pub struct SpriteDecl {
    pub name: String,
    pub xpos: (Align, i32),
    pub ypos: (Align, i32),
    params: Params,
    size: AutoSize,
    path: AppPath,
    sprite: Option<SpriteController>,
}


impl SpriteDecl {
    pub fn get_sprite(&self) -> Option<&SpriteController> {
        self.sprite.as_ref()
    }

    pub fn advance(&mut self, sensor: &Sensors, clipbank: &ClipBank) {
        if let Some(sprite) = self.sprite.as_mut() {
            sprite.advance(sensor, clipbank);
        }
    }

    /// Note that this function will consume the fields from the section if it recognizes them
    /// It will ease the caller to handle the unrecognized fields
    fn from_toml<'src>(name: &str, section: WithPos<&mut toml::Table<'src>>) -> Result<SpriteDecl, WithPos<SpriteDeclLoadReportKind>> {
        fn extract<'v, 'src, T: toml::ExtractValue<'src>>(entry: &'v toml::Entry<'src>) -> Result<&'v T, WithPos<toml::RetrieveError<'static>>> {
            entry.val.val.extract::<T>().map_err(|e| {
                entry.val.pos.with(toml::RetrieveError::IncompatibleType(e.expected, e.found))
            })
        }

        let WithPos { pos: section_pos, val: section } = section;

        let path = {
            let Some(entry) = section.pop("sprite") else {
                return Err(section_pos.with(SpriteDeclLoadReportKind::Retrieve(toml::RetrieveError::FieldNotFound("sprite"))));
            };

            let path = extract::<&str>(&entry)?;
            if path.is_empty() {
                return Err(entry.val.pos.with(SpriteDeclLoadReportKind::SpritePathEmtpy));
            }

            let path = Path::new(path);
            if path.is_absolute() {
                return Err(entry.val.pos.with(SpriteDeclLoadReportKind::SpritePatyAbsolute));
            }

            app_paths().sprite(path)
        };


        let size = {
            let width = section.pop("size.width");
            let height = section.pop("size.height");

            let width = if let Some(entry) = width {
                Some(*extract::<f64>(&entry)? as usize)
            } else { None };

            let height = if let Some(entry) = height {
                Some(*extract::<f64>(&entry)? as usize)
            } else { None };

            if width.is_none() || height.is_none() {
                return Err(section_pos.with(SpriteDeclLoadReportKind::NeedBothSize));
            }

            AutoSize::new(width, height)
        };

        let (xpos, ypos) = {
            let left   = section.pop("pos.left");
            let xcenter = section.pop("pos.xcenter");
            let right  = section.pop("pos.right");
            let xpos = match (left, xcenter, right) {
                (None, None, None) => return Err(WithPos { pos: section_pos, val: SpriteDeclLoadReportKind::NoHorizontalPosition }),
                (Some(entry), _, _) => (Align::Start, *extract::<f64>(&entry)? as i32),
                (None, Some(entry), _) => (Align::Center, *extract::<f64>(&entry)? as i32),
                (None, None, Some(entry)) => (Align::End, *extract::<f64>(&entry)? as i32),
            };

            let top   = section.pop("pos.top");
            let ycenter = section.pop("pos.ycenter");
            let bottom  = section.pop("pos.bottom");
            let ypos = match (top, ycenter, bottom) {
                (None, None, None) => return Err(WithPos { pos: section_pos, val: SpriteDeclLoadReportKind::NoVerticalPosition }),
                (Some(entry), _, _) => (Align::Start, *extract::<f64>(&entry)? as i32),
                (None, Some(entry), _) => (Align::Center, *extract::<f64>(&entry)? as i32),
                (None, None, Some(entry)) => (Align::End, *extract::<f64>(&entry)? as i32),
            };

            (xpos, ypos)
        };

        let params = section.pop_all_with_prefix("param.").map(|entry| -> Result<ParamEntry, WithPos<SpriteDeclLoadReportKind>> {
            Ok(ParamEntry {
                lineno: entry.key.pos.line,
                key: entry.key.val.strip_prefix("param.").unwrap().to_string(),
                val: extract::<&str>(&entry)?.to_string(),
            }) // TODO support other value types
        }).collect::<Result<Vec<_>, _>>()?;
        let params = Params::new(params);

        Ok(Self { size, xpos, ypos, path, params, name: name.into(), sprite: None })
    }

    // if the path is toml file, use it.
    // if the path is folder, look for sprite.toml in the folder.
    // if no sprite.toml, but only one toml, use it.
    // otherwise error.
    // clips are searched relatively to the sprite.toml file.
    fn load_sprite(&mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) -> Result<Option<SpriteController>, SpriteDeclLoadReportKind> {
        let filepath = if ! self.path.exists() {
            // path does not exist
            return Err(SpriteDeclLoadReportKind::NoSuchPath);
        } else if self.path.is_file() {
            // path exists and it is a file
            std::borrow::Cow::Borrowed(&self.path)
        } else if self.path.is_dir() {
            // path exists and it is a directory
            let Ok(files) = self.path.read_dir() else {
                return Err(SpriteDeclLoadReportKind::CannotReadDir);
            };
            let path = self.path.join("sprite.toml");
            let path = if path.is_file() {
                path
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
                        path
                    } else { // multiple toml files in the dir
                        return Err(SpriteDeclLoadReportKind::MultipleTomlInPath);
                    }
                } else { // no toml file in the dir
                    return Err(SpriteDeclLoadReportKind::NoTomlInPath);
                }
            };
            std::borrow::Cow::Owned(AppPath::try_from(path).unwrap())
        } else {
            return Err(SpriteDeclLoadReportKind::CannotHandlePath);
        };

        let sprite = SpriteController::load(&filepath, self.size, sensors, clipbank, &self.params)
            .map_err(SpriteDeclLoadReportKind::LoadIoError)?;
        Ok(self.sprite.replace(sprite))
    }

    pub fn unload_sprite(&mut self, sensors: &mut Sensors, clipbank: &mut ClipBank) {
        if let Some(sprite) = self.sprite.take() {
            sprite.unload(sensors, clipbank);
        }
    }
}

// impl Drop for SpriteDecl {
//     fn drop(&mut self) {
//         if self.sprite.is_some() { panic!("SpriteDecl must be manually 'unload'ed"); }
//     }
// }


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpriteId(u64);

#[derive(Debug)]
pub struct Sprites {
    decls: HashMap<u64, (SpriteDecl, usize)>, // lineno
    next_id: u64,
}

impl Sprites {
    pub fn new() -> Self {
        Self { next_id: 0, decls: HashMap::new() }
    }

    fn load_decls(path: &AppPath) -> (Self, String) {
        if ! path.exists() { return (Self::new(), String::new()) }

        let src_string = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                log_user!("{}", SpriteDeclLoadReport {
                    file: path, src: "", pos: Pos::nil(), kind: SpriteDeclLoadReportKind::IOError(e),
                });
                return (Self::new(), String::new());
            }
        };
        let src = src_string.as_str();

        let report_error = |e: WithPos<SpriteDeclLoadReportKind>| {
            log_user!("{}", SpriteDeclLoadReport { file: path, src, pos: e.pos, kind: e.val });
        };

        let mut toml = match toml::Parser::new(src).parse() {
            Ok(toml) => toml,
            Err(e) => {
                report_error(e.pos.with(SpriteDeclLoadReportKind::TomlParseError(e.val)));
                return (Self::new(), src_string);
            }
        };

        { // version
            let Some(version) = toml.pop("version") else {
                report_error(Pos::nil().with(SpriteDeclLoadReportKind::VersionMissing));
                return (Self::new(), src_string);
            };

            let pos = version.val.pos;

            let toml::Value::String(version) = version.val.val else {
                report_error(pos.with(SpriteDeclLoadReportKind::VersionNotString));
                return (Self::new(), src_string);
            };

            let Some(compat) = is_version_compatible(version) else {
                report_error(pos.with(SpriteDeclLoadReportKind::VersionUnrecognizable));
                return (Self::new(), src_string);
            };

            if ! compat {
                report_error(pos.with(SpriteDeclLoadReportKind::VersionNotCompatible));
                return (Self::new(), src_string);
            }
        }

        let mut decls = Self::new();
        for mut section_entry in toml.0 {
            let section = match section_entry.val.val.extract_mut::<toml::Table>() {
                Ok(section) => section,
                Err(_e) => {
                    report_error(section_entry.key.pos.with(SpriteDeclLoadReportKind::UnrecognizedEntry));
                    continue;
                }
            };

            let name = section_entry.key.val;
            let decl = match SpriteDecl::from_toml(name, WithPos { pos: section_entry.val.pos, val: section }) {
                Ok(decl) => decl,
                Err(e) => {
                    report_error(e);
                    continue;
                }
            };

            for entry in &section.0 {
                report_error(entry.key.pos.with(SpriteDeclLoadReportKind::UnrecognizedField));
            }

            decls.push(decl, section_entry.val.pos.line);
        }

        (decls, src_string)
    }

    fn push(&mut self, decl: SpriteDecl, lineno: usize) -> SpriteId {
        self.decls.insert(self.next_id, (decl, lineno));
        let id = self.next_id;
        self.next_id += 1;
        SpriteId(id)
    }

    pub fn get(&self, spriteid: SpriteId) -> Option<&SpriteDecl> {
        self.decls.get(&spriteid.0).map(|e| &e.0)
    }

    pub fn get_mut(&mut self, spriteid: SpriteId) -> Option<&mut SpriteDecl> {
        self.decls.get_mut(&spriteid.0).map(|e| &mut e.0)
    }

    pub fn load(file: &AppPath, sensors: &mut Sensors, clipbank: &mut ClipBank) -> Self {
        let (mut self_, src) = Self::load_decls(file);
        let src = src.as_str();

        for (_, (decl, lineno)) in self_.decls.iter_mut() {
            match decl.load_sprite(sensors, clipbank) {
                Ok(Some(scon)) => {
                    scon.unload(sensors, clipbank);
                }
                Err(e) => {
                    log_user!("{}", SpriteDeclLoadReport {
                        file, src: "", pos: Pos { line: *lineno, column: 0, span: Span::nil() },  kind: e,
                    });
                }
                _ => (), // Ok(None) // initialized for the first time
            }
        }

        self_
    }

    pub fn unload(&mut self, sensor: &mut Sensors, clipbank: &mut ClipBank) {
        log::debug!("Sprites unloaded, with {} decls", self.decls.len());
        for (_, (mut decl, _)) in self.decls.drain() {
            decl.unload_sprite(sensor, clipbank);
        }
    }

    pub fn reload(&mut self, file: &AppPath, sensors: &mut Sensors, clipbank: &mut ClipBank, mut report: impl FnMut(WindowUpdate)) {
        let (mut self_, src) = Self::load_decls(file);
        let src = src.as_str();

        const HIGH: i32 = 100_000;
        const MID:  i32 =   1_000;
        const LOW:  i32 =      10;

        fn cost(sp1: &SpriteDecl, sp2: &SpriteDecl) -> i32 {
            let mut score = 0; // re-usability score

            if sp1.path == sp2.path && sp1.size == sp2.size ||
                sp1.size.is_complete() && sp1.size == sp2.size {
                // can reuse the window buffer
                score += HIGH;
            }

            if sp1.path == sp2.path && sp1.params == sp2.params {
                // can reuse sprite controller
                score += MID;
            }

            if sp1.name == sp2.name {
                score += LOW;
            }

            - score
        }

        let m = self_.decls.len();
        let n = self.decls.len();

        let other_decls = self_.decls.keys().cloned().collect::<Vec<_>>();
        let cur_decls = self.decls.keys().cloned().collect::<Vec<_>>();

        let mut cost_mat = vec![0; n * m];
        for (j, other_key) in other_decls.iter().enumerate() {
            for (i, cur_key) in cur_decls.iter().enumerate() {
                let sp1 = &self_.decls[other_key].0;
                let sp2 = &self.decls[cur_key].0;
                cost_mat[j * n + i] = cost(sp1, sp2);
            }
        }

        // o3_hungarian requires # left vertices <= # right vertices.
        // We simply make phantom entries to right vertices (current decls) and assign MAX cost to all connected edges.
        // (all cost is negative so 0 is the maximum cost)
        // Since we can always find better matching from a matching containing phantom right vertices,
        // it is guaranteed that match_cur2new[n..] will have no match (they will have garbage values; we simply ignore them)
        let (_, match_cur2new) = crate::base::o3_hungarian(m, std::cmp::max(m, n), |j1, i1|
            if i1 <= n {
                cost_mat[(j1-1) * n + (i1-1)]
            } else {
                0 // maximum cost for phantom right vertex
            }
        );

        for (cur_decl_idx, &other_decl_idx1) in match_cur2new.iter().skip(1).take(n).enumerate() {
            let spriteid = SpriteId(cur_decls[cur_decl_idx]);
            let other_decl_idx = if other_decl_idx1 > 0 { other_decl_idx1 - 1} else {
                let (mut decl, _) = self.decls.remove(&cur_decls[cur_decl_idx]).unwrap();
                log::debug!("old sprite '{}' is matched with no new sprite", decl.name);
                decl.unload_sprite(sensors, clipbank);
                report(WindowUpdate { spriteid, kind: WindowUpdateKind::Delete });
                continue;
            };

            let (cur_decl, cur_decl_line) = self.decls.get_mut(&cur_decls[cur_decl_idx]).unwrap();
            let (other_decl, other_decl_line) = self_.decls.remove(&other_decls[other_decl_idx]).unwrap();
            log::debug!("old sprite '{}' is matched with new sprite '{}'", cur_decl.name, other_decl.name);

            let mut need_reschedule = false;
            let mut need_realloc = false;
            let mut need_redraw = false;
            let mut need_reload = false;

            cur_decl.name = other_decl.name;
            *cur_decl_line = other_decl_line;

            if cur_decl.xpos != other_decl.xpos || cur_decl.ypos != other_decl.ypos {
                cur_decl.xpos = other_decl.xpos;
                cur_decl.ypos = other_decl.ypos;
                need_redraw = true;
            }

            let need_realloc =
                cur_decl.path == other_decl.path && cur_decl.size == other_decl.size ||
                cur_decl.size.is_complete() && cur_decl.size != other_decl.size;

            cur_decl.size = other_decl.size;

            if cur_decl.path != other_decl.path || cur_decl.params != other_decl.params {
                cur_decl.path = other_decl.path;
                cur_decl.params = other_decl.params;
                need_reload = true;
                need_redraw = true;
                need_reschedule = true;
            }

            // FIXME there's no way to only realloc the clips, so we just do full-reload
            if need_reload || need_realloc {
                log::debug!(" --> reloading sprite controller '{}'", cur_decl.path);
                match cur_decl.load_sprite(sensors, clipbank) {
                    Ok(Some(scon)) => {
                        scon.unload(sensors, clipbank);
                    }
                    Err(e) => {
                        log_user!("{}", SpriteDeclLoadReport {
                            file, src: "", pos: Pos { line: *cur_decl_line, column: 0, span: Span::nil() }, kind: e,
                        });
                    }
                    _ => (), // Ok(None)
                }
            }

            // if need_reschedule {
            //     report(WindowUpdate { spriteid, kind: WindowUpdateKind::Reschedule });
            // }

            // if need_realloc {
            //     report(WindowUpdate { spriteid, kind: WindowUpdateKind::ModSize });
            // }

            // if need_redraw {
            //     report(WindowUpdate { spriteid, kind: WindowUpdateKind::Redraw });
            // }

            // window renderer could read pixels from updated sprite frame before it processes DELETE & CREATE event.
            // In case the size have increased, window renderer's buffer (being not being updated from small size) may overflow.
            // To prevent it, we assign completely new sprite id for the updated sprite.
            // There was a plan to reuse window buffers, sprite resources for partial updates but it didn't go well, maybe in the future.
            report(WindowUpdate { spriteid, kind: WindowUpdateKind::Delete });
            let (decl, lineno) = self.decls.remove(&cur_decls[cur_decl_idx]).unwrap();
            let spriteid = self.push(decl, lineno);
            report(WindowUpdate { spriteid, kind: WindowUpdateKind::Create });
        }

        for (_, (mut decl, lineno)) in self_.decls {
            log::debug!("new sprite '{}' added", decl.name);
            log::debug!(" --> reloading sprite controller '{}'", decl.path);
            let success = match decl.load_sprite(sensors, clipbank) {
                Ok(Some(_)) => unreachable!(),
                Ok(None) => true,
                Err(e) => {
                    log_user!("{}", SpriteDeclLoadReport {
                        file, src: "", pos: Pos { line: lineno, column: 0, span: Span::nil() }, kind: e,
                    });
                    false
                }
            };
            let spriteid = self.push(decl, lineno);
            if success {
                report(WindowUpdate { spriteid, kind: WindowUpdateKind::Create });
            }
        }
    }

    pub fn reload_sprite(&mut self, sprite_path: &AppPath, sensors: &mut Sensors, clipbank: &mut ClipBank, mut report: impl FnMut(WindowUpdate)) {
        let file = app_paths().sprite_list();
        let file = &file;
        let containing_path = sprite_path.parent().unwrap();

        for (&id, (decl, lineno)) in &mut self.decls {
            let spriteid = SpriteId(id);
            let path_match = if decl.path.is_file() {
                &decl.path == sprite_path
            } else {
                decl.path == containing_path
            };
            if ! path_match { continue; }
            log::debug!("reloading sprite decl '{}'", decl.path);

            match decl.load_sprite(sensors, clipbank) {
                Ok(Some(scon)) => {
                    scon.unload(sensors, clipbank);
                    report(WindowUpdate { spriteid, kind: WindowUpdateKind::Delete });
                    report(WindowUpdate { spriteid, kind: WindowUpdateKind::Create });
                }
                Ok(None) => {
                    report(WindowUpdate { spriteid, kind: WindowUpdateKind::Create });
                }
                Err(e) => {
                    log_user!("{}", SpriteDeclLoadReport {
                        file, src: "", pos: Pos { line: *lineno, column: 0, span: Span::nil() }, kind: e,
                    });
                }
            }
        }
    }

    pub fn ids(&self) -> impl Iterator<Item=SpriteId> + use<'_> {
        self.decls.keys().map(|k| SpriteId(*k))
    }

    pub fn iter(&self) -> impl Iterator<Item=(SpriteId, &(SpriteDecl, usize))> {
        self.decls.iter().map(|(k,v)| (SpriteId(*k), v))
    }
}

// impl Drop for Sprites {
//     fn drop(&mut self) {
//         if ! self.decls.is_empty() { panic!("Sprites must be manually 'unload'ed"); }
//     }
// }