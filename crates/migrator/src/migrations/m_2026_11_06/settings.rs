use anyhow::Result;
use serde_json::Value;

use crate::migrations::migrate_settings;

const SETTINGS_KEY: &str = "settings";
const TITLE_BAR_KEY: &str = "title_bar";
const STATUS_BAR_KEY: &str = "status_bar";

/// The project name, worktree and branch moved from the title bar to the
/// status bar, so their settings move with them.
const MOVED_KEYS: [&str; 4] = [
    "show_project_items",
    "show_worktree_name",
    "show_branch_name",
    "show_branch_status_icon",
];

/// The user menu and the sign in button were removed from the title bar.
const REMOVED_KEYS: [&str; 3] = ["show_user_picture", "show_user_menu", "show_sign_in"];

pub fn move_project_items_to_status_bar(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut migrate_one)
}

fn migrate_one(object: &mut serde_json::Map<String, Value>) -> Result<()> {
    migrate_title_bar(object);

    if let Some(settings) = object
        .get_mut(SETTINGS_KEY)
        .and_then(|value| value.as_object_mut())
    {
        migrate_title_bar(settings);
    }

    Ok(())
}

fn migrate_title_bar(object: &mut serde_json::Map<String, Value>) {
    let Some(title_bar) = object
        .get_mut(TITLE_BAR_KEY)
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };

    for key in REMOVED_KEYS {
        title_bar.remove(key);
    }

    let moved: Vec<(&str, Value)> = MOVED_KEYS
        .iter()
        .filter_map(|key| Some((*key, title_bar.remove(*key)?)))
        .collect();
    let title_bar_is_empty = title_bar.is_empty();
    if title_bar_is_empty {
        object.remove(TITLE_BAR_KEY);
    }
    if moved.is_empty() {
        return;
    }

    let status_bar = object
        .entry(STATUS_BAR_KEY)
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(status_bar) = status_bar.as_object_mut() else {
        return;
    };
    for (key, value) in moved {
        // A value already set in the status bar wins over the old one.
        status_bar.entry(key).or_insert(value);
    }
}
