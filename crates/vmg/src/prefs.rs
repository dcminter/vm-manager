//! What the window remembers between runs, kept in `GSettings`.

use std::collections::{BTreeMap, HashMap};

use gtk::gio;
use gtk::prelude::{SettingsExt, SettingsExtManual};

const SCHEMA_ID: &str = "com.paperstack.VmManager";
const WINDOW_WIDTH: &str = "window-width";
const WINDOW_HEIGHT: &str = "window-height";
const SIDEBAR_WIDTH: &str = "sidebar-width";
const SIDEBAR_VISIBLE: &str = "sidebar-visible";
const SHOW_STOPPED: &str = "show-stopped-machines";
const SHOW_REMOTE: &str = "show-remote-images";
const COLUMN_WIDTHS: &str = "column-widths";
const BUILT_SCHEMA_DIR: &str = env!("VMG_SCHEMA_DIR");

pub const MIN_SIDEBAR_WIDTH: i32 = 180;
pub const MAX_SIDEBAR_WIDTH: i32 = 900;

/// Widths by table, then by column title.
pub type ColumnWidths = BTreeMap<String, BTreeMap<String, i32>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub window_width: i32,
    pub window_height: i32,
    pub sidebar_width: i32,
    pub sidebar_visible: bool,
    pub show_stopped_machines: bool,
    pub show_remote_images: bool,
    pub column_widths: ColumnWidths,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window_width: 1200,
            window_height: 760,
            sidebar_width: 280,
            sidebar_visible: false,
            show_stopped_machines: true,
            show_remote_images: true,
            column_widths: ColumnWidths::new(),
        }
    }
}

impl Settings {
    /// Values brought into the ranges the schema accepts.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.window_width = self.window_width.clamp(600, 10_000);
        self.window_height = self.window_height.clamp(400, 10_000);
        self.sidebar_width = self
            .sidebar_width
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
        self
    }

    /// Records a width, saying whether it differs from what was held.
    pub fn set_column_width(&mut self, table: &str, column: &str, width: i32) -> bool {
        let columns = self.column_widths.entry(table.to_owned()).or_default();
        columns.insert(column.to_owned(), width) != Some(width)
    }

    pub fn column_width(&self, table: &str, column: &str) -> Option<i32> {
        self.column_widths.get(table)?.get(column).copied()
    }
}

/// The settings store, or nothing when no schema could be found.
pub struct Prefs {
    settings: Option<gio::Settings>,
}

impl Prefs {
    pub fn new() -> Self {
        Self::open(None)
    }

    /// A store that forgets at exit, so tests never touch the user's settings.
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::open(Some(&gio::functions::memory_settings_backend_new()))
    }

    fn open(backend: Option<&gio::SettingsBackend>) -> Self {
        let Some(schema) = lookup_schema() else {
            crate::warn(&format!(
                "no {SCHEMA_ID} settings schema found; preferences will not be kept"
            ));
            return Self { settings: None };
        };
        Self {
            settings: Some(gio::Settings::new_full(&schema, backend, None)),
        }
    }

    pub fn load(&self) -> Settings {
        let Some(settings) = &self.settings else {
            return Settings::default();
        };
        Settings {
            window_width: settings.int(WINDOW_WIDTH),
            window_height: settings.int(WINDOW_HEIGHT),
            sidebar_width: settings.int(SIDEBAR_WIDTH),
            sidebar_visible: settings.boolean(SIDEBAR_VISIBLE),
            show_stopped_machines: settings.boolean(SHOW_STOPPED),
            show_remote_images: settings.boolean(SHOW_REMOTE),
            column_widths: read_widths(settings),
        }
        .clamped()
    }

    /// Each key is written on its own, so one refused value cannot lose the rest.
    pub fn store(&self, settings: &Settings) {
        let Some(store) = &self.settings else {
            return;
        };
        let settings = settings.clone().clamped();
        report(
            WINDOW_WIDTH,
            store.set_int(WINDOW_WIDTH, settings.window_width),
        );
        report(
            WINDOW_HEIGHT,
            store.set_int(WINDOW_HEIGHT, settings.window_height),
        );
        report(
            SIDEBAR_WIDTH,
            store.set_int(SIDEBAR_WIDTH, settings.sidebar_width),
        );
        report(
            SIDEBAR_VISIBLE,
            store.set_boolean(SIDEBAR_VISIBLE, settings.sidebar_visible),
        );
        report(
            SHOW_STOPPED,
            store.set_boolean(SHOW_STOPPED, settings.show_stopped_machines),
        );
        report(
            SHOW_REMOTE,
            store.set_boolean(SHOW_REMOTE, settings.show_remote_images),
        );
        report(
            COLUMN_WIDTHS,
            store.set(COLUMN_WIDTHS, write_widths(&settings.column_widths)),
        );
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self::new()
    }
}

fn report(key: &str, outcome: Result<(), gtk::glib::BoolError>) {
    if let Err(error) = outcome {
        crate::warn(&format!("could not store {key}: {error}"));
    }
}

fn write_widths(widths: &ColumnWidths) -> HashMap<String, HashMap<String, i32>> {
    widths
        .iter()
        .map(|(table, columns)| {
            let columns = columns
                .iter()
                .map(|(column, width)| (column.clone(), *width))
                .collect();
            (table.clone(), columns)
        })
        .collect()
}

fn read_widths(settings: &gio::Settings) -> ColumnWidths {
    let Some(stored) = settings
        .value(COLUMN_WIDTHS)
        .get::<HashMap<String, HashMap<String, i32>>>()
    else {
        return ColumnWidths::new();
    };
    stored
        .into_iter()
        .map(|(table, columns)| (table, columns.into_iter().collect()))
        .collect()
}

/// The installed schema, or the one compiled into the build directory.
fn lookup_schema() -> Option<gio::SettingsSchema> {
    if let Some(source) = gio::SettingsSchemaSource::default()
        && let Some(schema) = source.lookup(SCHEMA_ID, true)
    {
        return Some(schema);
    }
    match gio::SettingsSchemaSource::from_directory(
        BUILT_SCHEMA_DIR,
        gio::SettingsSchemaSource::default().as_ref(),
        true,
    ) {
        Ok(source) => source.lookup(SCHEMA_ID, true),
        Err(error) => {
            crate::warn(&format!(
                "could not read schemas from {BUILT_SCHEMA_DIR}: {error}"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn scratch() -> Prefs {
        let backend = gio::functions::memory_settings_backend_new();
        let prefs = Prefs::open(Some(&backend));
        assert!(prefs.settings.is_some());
        prefs
    }

    #[test]
    fn an_untouched_store_reads_back_as_the_defaults() {
        assert_eq!(scratch().load(), Settings::default());
        assert!(!Settings::default().sidebar_visible);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let prefs = scratch();
        let mut stored = Settings {
            window_width: 900,
            window_height: 700,
            sidebar_width: 421,
            sidebar_visible: true,
            show_stopped_machines: false,
            show_remote_images: false,
            column_widths: ColumnWidths::new(),
        };
        assert!(stored.set_column_width("machines", "Image", 240));
        assert!(!stored.set_column_width("machines", "Image", 240));
        prefs.store(&stored);
        assert_eq!(prefs.load(), stored);
        assert_eq!(prefs.load().column_width("machines", "Image"), Some(240));
    }

    #[test]
    fn an_absurd_width_is_brought_into_range() {
        let prefs = scratch();
        prefs.store(&Settings {
            sidebar_width: 99_999,
            ..Settings::default()
        });
        assert_eq!(prefs.load().sidebar_width, MAX_SIDEBAR_WIDTH);
    }

    #[test]
    fn a_store_without_a_schema_answers_with_defaults() {
        let prefs = Prefs { settings: None };
        prefs.store(&Settings {
            sidebar_width: 421,
            ..Settings::default()
        });
        assert_eq!(prefs.load(), Settings::default());
    }
}
