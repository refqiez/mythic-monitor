use crate::parser::{toml, WithPos, Pos};
use crate::base::AppPath;

use std::path::Path;
use std::collections::HashSet;

pub enum VersionError {
    Missing,
    NonString,
    Unrecognizable,
    NotCompatible,
}

pub enum GeneralSchemaError {
    Version(VersionError),
    UnrecognizedGlobalField,
    UnrecognizedField,
    Retrieve(toml::RetrieveError),
    PathEmpty,
    PathAbsolute,
}

pub struct TomlTableAccessor<'src, 'table, 'key> {
    pub inner: &'table toml::Table<'src>,
    pub accessed_keys: HashSet<&'key str>,
}

impl<'src, 'table, 'key> TomlTableAccessor<'src, 'table, 'key> {
    pub fn new(table: &'table toml::Table<'src>) -> Self {
        Self { inner: table, accessed_keys: HashSet::new() }
    }

    pub fn mark_accessed(&mut self, key: &'key str) {
        self.accessed_keys.insert(key);
    }

    pub fn check_version(&mut self) -> Result<(), WithPos<GeneralSchemaError>>{
        use VersionError::*;
        use GeneralSchemaError::Version;

        self.mark_accessed("version");
        let Some(version) = self.inner.get("version") else {
            return Err(Pos::nil().with(Version(Missing)));
        };

        let pos = version.val.pos;

        let toml::Value::String(version) = version.val.val else {
            return Err(pos.with(Version(NonString)));
        };

        let Some(compat) = crate::base::is_version_compatible(version) else {
            return Err(pos.with(Version(Unrecognizable)));
        };

        if ! compat {
            return Err(pos.with(Version(NotCompatible)));
        }

        Ok(())
    }

    pub fn retrieve<'t, T: toml::ExtractValue<'src> + ?Sized>(&'t mut self, key: &'key str, parent_pos: Option<Pos>) -> Result<Option<WithPos<&'t T>>, WithPos<toml::RetrieveError>> {
        self.mark_accessed(key);
        self.inner.retrieve(key, parent_pos)
    }

    pub fn retrieve_noneg<'t>(&'t mut self, key: &'key str, parent_pos: Option<Pos>) -> Result<Option<WithPos<&'t f64>>, WithPos<toml::RetrieveError>> {
        self.mark_accessed(key);
        self.inner.retrieve_noneg(key, parent_pos)
    }

    pub fn accessed(&self, key: &'key str) -> bool {
        self.accessed_keys.contains(key)
    }
}

pub trait LoaderHelper<'src, 'caller, Error> where
    toml::RetrieveError: Into<Error>,
    GeneralSchemaError: Into<Error>,
{
    fn err<T: Into<Error>>(&self, e: WithPos<T>);
    fn to_app_path(&self, path: &Path) -> AppPath;

    fn get_optional<T: toml::ExtractValue<'src> + Copy>(&self, table: &mut TomlTableAccessor<'src, 'caller, 'src>, key: &'src str) -> Option<T> {
        match table.retrieve::<T>(key, None) {
            Ok(v) => v.map(|v| *v.val),
            Err(e) => {
                self.err(e);
                None // placeholder
            }
        }
    }

    fn get_optional_noneg(&self, table: &mut TomlTableAccessor<'src, 'caller, 'src>, key: &'src str) -> Option<f64> {
        match table.retrieve_noneg(key, None) {
            Ok(v) => v.map(|v| *v.val),
            Err(e) => {
                self.err(e);
                None // placeholder
            }
        }
    }

    fn get_default<T: toml::ExtractValue<'src> + Copy>(&self, table: &mut TomlTableAccessor<'src, 'caller, 'src>, key: &'src str, default: T) -> T {
        match table.retrieve::<T>(key, None) {
            Ok(Some(v)) => *v.val,
            Ok(None) => default,
            Err(e) => {
                self.err(e);
                default // placeholder
            }
        }
    }

    fn get_default_noneg(&self, table: &mut TomlTableAccessor<'src, 'caller, 'src>, key: &'src str, default: f64) -> f64 {
        match table.retrieve_noneg(key, None) {
            Ok(Some(v)) => *v.val,
            Ok(None) => default,
            Err(e) => {
                self.err(e);
                default // placeholder
            }
        }
    }

    fn file_unrecognized(&self, table: TomlTableAccessor<'src, 'caller, 'src>) {
        for entry in &table.inner.0 {
            if table.accessed(entry.key.val) { continue; }
            self.err(entry.key.pos.with(GeneralSchemaError::UnrecognizedField));
        }
    }

    fn validate_path(&self, path: &str) -> Result<AppPath, GeneralSchemaError> {
        let path = Path::new(path.trim());
        if path == Path::new("") {
            Err(GeneralSchemaError::PathEmpty)
        } else if path.is_absolute() {
            Err(GeneralSchemaError::PathAbsolute)
        } else {
            Ok(self.to_app_path(path))
        }
    }
}