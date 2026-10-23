use std::path::PathBuf;

use gpui_kit::{App, WindowAppearance};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// Sets the appearance of every window. The theme follows it through each window's
    /// appearance observer, so this is the only place the choice takes effect.
    pub fn apply(self, cx: &App) {
        cx.set_window_appearance(match self {
            Self::System => None,
            Self::Light => Some(WindowAppearance::Light),
            Self::Dark => Some(WindowAppearance::Dark),
        });
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
}

impl Settings {
    /// Falls back to defaults when the file is missing or unreadable.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                eprintln!("ignoring {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let written = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|()| std::fs::write(&path, serde_json::to_vec_pretty(self).unwrap()));
        if let Err(e) = written {
            eprintln!("could not save {}: {e}", path.display());
        }
    }
}

fn path() -> Option<PathBuf> {
    std::env::home_dir()
        .map(|home| home.join("Library/Application Support/com.rlejnieks.spacee/settings.json"))
}
