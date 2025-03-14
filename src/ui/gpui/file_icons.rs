use std::{path::Path, str};

use gpui::{AppContext, AssetSource, Global, SharedString};
use serde_derive::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Debug)]
struct TypeConfig {
    icon: SharedString,
}

#[derive(Deserialize, Debug)]
pub struct FileIcons {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    types: HashMap<String, TypeConfig>,
}

impl Global for FileIcons {}

const COLLAPSED_DIRECTORY_TYPE: &str = "collapsed_folder";
const EXPANDED_DIRECTORY_TYPE: &str = "expanded_folder";
const FILE_TYPES_ASSET: &str = "icons/file_icons/file_types.json";

pub fn init(assets: impl AssetSource, cx: &mut AppContext) {
    cx.set_global(FileIcons::new(assets))
}

impl FileIcons {
    pub fn get(cx: &AppContext) -> &Self {
        cx.global::<FileIcons>()
    }

    pub fn new(assets: impl AssetSource) -> Self {
        assets
            .load("icons/file_icons/file_types.json")
            .ok()
            .flatten()
            .and_then(|file| serde_json::from_str::<FileIcons>(str::from_utf8(&file).unwrap()).ok())
            .unwrap_or_else(|| FileIcons {
                stems: HashMap::default(),
                suffixes: HashMap::default(),
                types: HashMap::default(),
            })
    }

    pub fn get_icon(path: &Path, cx: &AppContext) -> Option<SharedString> {
        let this = cx.try_global::<Self>()?;

        // Try to find icon by file stem first
        if let Some(filename) = path.file_name() {
            if let Some(filename_str) = filename.to_str() {
                if let Some(type_str) = this.stems.get(filename_str) {
                    return this.get_type_icon(type_str);
                }
            }
        }

        // Then try by extension
        if let Some(extension) = path.extension() {
            if let Some(ext_str) = extension.to_str() {
                if let Some(type_str) = this.suffixes.get(ext_str) {
                    return this.get_type_icon(type_str);
                }
            }
        }

        // Default file icon
        this.get_type_icon("default")
    }

    pub fn get_type_icon(&self, typ: &str) -> Option<SharedString> {
        self.types
            .get(typ)
            .map(|type_config| type_config.icon.clone())
    }

    pub fn get_type_icon_static(typ: &str, cx: &AppContext) -> Option<SharedString> {
        let this = cx.try_global::<Self>()?;
        this.get_type_icon(typ)
    }

    pub fn get_folder_icon(expanded: bool, cx: &AppContext) -> Option<SharedString> {
        let this = cx.try_global::<Self>()?;

        let key = if expanded {
            EXPANDED_DIRECTORY_TYPE
        } else {
            COLLAPSED_DIRECTORY_TYPE
        };

        this.get_type_icon(key)
    }
}
