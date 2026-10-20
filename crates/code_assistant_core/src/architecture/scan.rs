//! The deterministic half of the overview: the component tree of a project
//! and the dependencies between components, built from file paths,
//! manifests and the facts of [`super::extract`] — no model involved.
//!
//! Components are directories and files, with a few rules that make the
//! tree look like the architecture rather than the file system:
//! - a package's `src` folder is transparent (its modules are the
//!   package's parts),
//! - a folder that only groups packages (`crates/`, `packages/`) is
//!   transparent too,
//! - a chain of folders without own files collapses into one component,
//! - entry files (`lib.rs`, `mod.rs`, `index.ts`, `__init__.py`) and a Rust
//!   module file next to its folder (`foo.rs` + `foo/`) belong to the folder.

use super::extract::{FileFacts, Import, Language};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Folders never part of the architecture, whatever gitignore says.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "vendor",
    "coverage",
    "site-packages",
    ".git",
];

const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "setup.py"];

/// Whether a project-relative path can matter for the overview: a source
/// file or a manifest outside the skipped folders.
pub fn is_relevant_path(path: &str) -> bool {
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return MANIFESTS.contains(&part)
                || part.eq_ignore_ascii_case("readme.md")
                || (Language::from_path(part).is_some() && !part.ends_with(".min.js"));
        }
        if SKIPPED_DIRS.contains(&part) {
            return false;
        }
    }
    false
}

pub fn is_manifest(file_name: &str) -> bool {
    MANIFESTS.contains(&file_name)
}

/// Test code: shown, but nothing depends on it.
fn is_test_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    matches!(
        stem,
        "tests" | "test" | "__tests__" | "test_support" | "testing" | "mocks" | "conftest"
    ) || stem.ends_with("_tests")
        || stem.ends_with("_test")
        || stem.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
}

fn is_entry_file(name: &str) -> bool {
    matches!(
        name,
        "lib.rs"
            | "mod.rs"
            | "index.ts"
            | "index.tsx"
            | "index.js"
            | "index.jsx"
            | "index.mjs"
            | "__init__.py"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKind {
    /// The project itself.
    Project,
    /// A crate, npm package or Python project (has a manifest).
    Package,
    /// A folder of code.
    Module,
    /// A single source file.
    File,
}

impl ComponentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Package => "package",
            Self::Module => "module",
            Self::File => "file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    Cargo,
    Npm,
    Python,
}

/// What a manifest says about its package.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub kind: ManifestKind,
    /// Declares a package (a Cargo workspace root alone does not).
    pub is_package: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Names of the declared (non-dev) dependencies.
    pub dependencies: Vec<String>,
}

impl Manifest {
    /// Parse a manifest file by its name; `None` for other files.
    pub fn parse(file_name: &str, text: &str) -> Option<Self> {
        match file_name {
            "Cargo.toml" => Some(parse_cargo(text)),
            "package.json" => Some(parse_package_json(text)),
            "pyproject.toml" => Some(parse_pyproject(text)),
            "setup.py" => Some(Self {
                kind: ManifestKind::Python,
                is_package: true,
                name: None,
                description: None,
                dependencies: Vec::new(),
            }),
            _ => None,
        }
    }
}

/// `key = "value"` → `value`.
fn toml_string(line: &str, key: &str) -> Option<String> {
    let rest = line
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix('=')?
        .trim();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

fn parse_cargo(text: &str) -> Manifest {
    let mut manifest = Manifest {
        kind: ManifestKind::Cargo,
        is_package: false,
        name: None,
        description: None,
        dependencies: Vec::new(),
    };
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
            if section == "package" {
                manifest.is_package = true;
            }
            // `[dependencies.foo]`
            if let Some(dep) = section.strip_prefix("dependencies.") {
                manifest.dependencies.push(dep.to_owned());
            }
            continue;
        }
        if section == "package" {
            if let Some(name) = toml_string(line, "name") {
                manifest.name = Some(name);
            } else if let Some(description) = toml_string(line, "description") {
                manifest.description = Some(description);
            }
        } else if (section == "dependencies"
            || section == "build-dependencies"
            || section.ends_with(".dependencies") && !section.ends_with("dev-dependencies"))
            && line.contains('=')
            && let Some(key) = line.split(['=', '.']).next()
        {
            // Continuation lines of a multi-line value have no plain key.
            let key = key.trim().trim_matches('"');
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                manifest.dependencies.push(key.to_owned());
            }
        }
    }
    manifest
}

fn parse_package_json(text: &str) -> Manifest {
    let json: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let string = |key: &str| json.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    Manifest {
        kind: ManifestKind::Npm,
        is_package: true,
        name: string("name"),
        description: string("description"),
        dependencies: json
            .get("dependencies")
            .and_then(|d| d.as_object())
            .map(|d| d.keys().cloned().collect())
            .unwrap_or_default(),
    }
}

fn parse_pyproject(text: &str) -> Manifest {
    let mut manifest = Manifest {
        kind: ManifestKind::Python,
        is_package: true,
        name: None,
        description: None,
        dependencies: Vec::new(),
    };
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
        } else if section == "project" || section == "tool.poetry" {
            if let Some(name) = toml_string(line, "name") {
                manifest.name.get_or_insert(name);
            } else if let Some(description) = toml_string(line, "description") {
                manifest.description.get_or_insert(description);
            }
        }
    }
    manifest
}

/// The first prose paragraph of a README: no headings, badges or HTML.
pub fn readme_paragraph(text: &str) -> Option<String> {
    let mut paragraph = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let skip = line.starts_with('#')
            || line.starts_with('<')
            || line.starts_with("[![")
            || line.starts_with("![")
            || line.starts_with("```")
            || line.starts_with("---");
        if line.is_empty() || skip {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        paragraph.push(line);
    }
    let text = paragraph.join(" ");
    (!text.is_empty()).then(|| text.chars().take(500).collect())
}

/// A source file with its facts, the input of [`build_model`].
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// `/`-separated, relative to the project root.
    pub path: String,
    pub language: Language,
    pub facts: Arc<FileFacts>,
}

/// One box of the overview.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    /// The project-relative path of its folder or file; `""` for the
    /// project.
    pub id: String,
    pub name: String,
    pub kind: ComponentKind,
    pub language: Option<Language>,
    pub parent: Option<String>,
    pub children: Vec<String>,
    /// The manifest description, README paragraph or module doc.
    pub description: Option<String>,
    /// Public items of its entry files (or of the file itself).
    pub interface: Vec<String>,
    /// Dependencies its manifest declares (packages only).
    pub external_deps: Vec<String>,
    /// Files that belong to it directly (entry files, its own file).
    pub own_files: Vec<String>,
    /// Test code (or below a test folder).
    pub is_test: bool,
    pub file_count: usize,
    pub line_count: usize,
}

/// `from` uses `to`; both are children of the same component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// How many references back it.
    pub weight: usize,
}

/// The component tree of one project with its dependency edges.
#[derive(Debug, Clone, Default)]
pub struct ArchitectureModel {
    pub root: PathBuf,
    pub components: HashMap<String, Component>,
    /// Edges between the children of a component, keyed by that component.
    pub edges: HashMap<String, Vec<Edge>>,
    /// More files than the scan reads; the overview is partial.
    pub truncated: bool,
}

impl ArchitectureModel {
    pub fn get(&self, id: &str) -> Option<&Component> {
        self.components.get(id)
    }

    /// The edges between the children of `id`.
    pub fn level_edges(&self, id: &str) -> &[Edge] {
        self.edges.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Siblings `id` uses, heaviest first.
    pub fn uses(&self, id: &str) -> Vec<&str> {
        let Some(parent) = self.get(id).and_then(|c| c.parent.as_deref()) else {
            return Vec::new();
        };
        let mut edges: Vec<&Edge> = self
            .level_edges(parent)
            .iter()
            .filter(|e| e.from == id)
            .collect();
        edges.sort_by_key(|e| std::cmp::Reverse(e.weight));
        edges.into_iter().map(|e| e.to.as_str()).collect()
    }

    /// Siblings that use `id`, heaviest first.
    pub fn used_by(&self, id: &str) -> Vec<&str> {
        let Some(parent) = self.get(id).and_then(|c| c.parent.as_deref()) else {
            return Vec::new();
        };
        let mut edges: Vec<&Edge> = self
            .level_edges(parent)
            .iter()
            .filter(|e| e.to == id)
            .collect();
        edges.sort_by_key(|e| std::cmp::Reverse(e.weight));
        edges.into_iter().map(|e| e.from.as_str()).collect()
    }

    /// `id` and its ancestors up to the project, project first.
    pub fn ancestry(&self, id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = self.get(id);
        while let Some(component) = current {
            chain.push(component.id.clone());
            current = component.parent.as_deref().and_then(|p| self.get(p));
        }
        chain.reverse();
        chain
    }

    pub fn name<'a>(&'a self, id: &'a str) -> &'a str {
        self.get(id).map_or(id, |c| c.name.as_str())
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn parent_dir(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[derive(Default)]
struct Dir {
    files: Vec<String>,
    dirs: BTreeMap<String, Dir>,
}

impl Dir {
    fn insert(&mut self, path: &str) {
        match path.split_once('/') {
            Some((head, rest)) => self.dirs.entry(head.to_owned()).or_default().insert(rest),
            None => self.files.push(path.to_owned()),
        }
    }

    /// Sub-folders holding source somewhere below.
    fn source_dirs(&self) -> impl Iterator<Item = (&String, &Dir)> {
        self.dirs.iter().filter(|(_, d)| d.has_source())
    }

    fn has_source(&self) -> bool {
        !self.files.is_empty() || self.dirs.values().any(Dir::has_source)
    }
}

/// Build the component tree and its edges.
pub fn build_model(
    root: PathBuf,
    files: Vec<SourceFile>,
    manifests: HashMap<String, Manifest>,
    readmes: HashMap<String, String>,
    truncated: bool,
) -> ArchitectureModel {
    let mut tree = Dir::default();
    for file in &files {
        tree.insert(&file.path);
    }
    let by_path: HashMap<&str, &SourceFile> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut builder = Builder {
        manifests: &manifests,
        readmes: &readmes,
        files: &by_path,
        components: HashMap::new(),
    };
    let project_name = manifests
        .get("")
        .filter(|m| m.is_package)
        .and_then(|m| m.name.clone())
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into());
    builder.dir_component(
        String::new(),
        project_name,
        &tree,
        None,
        ComponentKind::Project,
    );
    let mut components = builder.components;
    for id in components.keys().cloned().collect::<Vec<_>>() {
        if components[&id].parent.is_none() {
            totals(&mut components, &id, &by_path);
        }
    }
    let ids: Vec<String> = components.keys().cloned().collect();
    for id in ids {
        let mut parent = components[&id].parent.clone();
        while let Some(p) = parent {
            if components[&p].is_test {
                components.get_mut(&id).unwrap().is_test = true;
                break;
            }
            parent = components[&p].parent.clone();
        }
    }

    let resolver = Resolver::new(&files, &tree, &manifests);
    let mut owner: HashMap<String, String> = HashMap::new();
    for component in components.values() {
        for file in &component.own_files {
            owner.insert(file.clone(), component.id.clone());
        }
    }
    let model = ArchitectureModel {
        root,
        components,
        edges: HashMap::new(),
        truncated,
    };
    let mut weights: HashMap<(String, String, String), usize> = HashMap::new();
    let mut chains: HashMap<String, Vec<String>> = HashMap::new();
    let mut chain_of = |id: &str| {
        chains
            .entry(id.to_owned())
            .or_insert_with(|| model.ancestry(id))
            .clone()
    };
    for file in &files {
        let Some(from) = owner.get(&file.path) else {
            continue;
        };
        let targets = resolver.resolve(file);
        if targets.is_empty() {
            continue;
        }
        let from_chain = chain_of(from);
        let mut seen = HashSet::new();
        for target in targets {
            let Some(to) = owning_component(&model, &owner, &target) else {
                continue;
            };
            // Code only references test helpers from its own tests.
            if model.components[&to].is_test {
                continue;
            }
            let to_chain = chain_of(&to);
            let common = from_chain
                .iter()
                .zip(&to_chain)
                .take_while(|(a, b)| a == b)
                .count();
            // A reference into its own ancestor or descendant is internal.
            if common == 0 || common >= from_chain.len() || common >= to_chain.len() {
                continue;
            }
            let key = (
                from_chain[common - 1].clone(),
                from_chain[common].clone(),
                to_chain[common].clone(),
            );
            if seen.insert(key.clone()) {
                *weights.entry(key).or_default() += 1;
            }
        }
    }
    let mut model = model;
    for ((parent, from, to), weight) in weights {
        model
            .edges
            .entry(parent)
            .or_default()
            .push(Edge { from, to, weight });
    }
    for edges in model.edges.values_mut() {
        edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    }
    model
}

/// The component a resolved path belongs to: the owner of the file, or the
/// nearest component folder at or above it.
fn owning_component(
    model: &ArchitectureModel,
    owner: &HashMap<String, String>,
    path: &str,
) -> Option<String> {
    if let Some(id) = owner.get(path) {
        return Some(id.clone());
    }
    let mut path = path;
    loop {
        if model.components.contains_key(path) {
            return Some(path.to_owned());
        }
        if path.is_empty() {
            return None;
        }
        path = parent_dir(path);
    }
}

/// Sum files, lines and the dominant language bottom-up.
fn totals(
    components: &mut HashMap<String, Component>,
    id: &str,
    files: &HashMap<&str, &SourceFile>,
) -> (usize, usize, HashMap<Language, usize>) {
    let children = components[id].children.clone();
    let mut file_count = 0;
    let mut line_count = 0;
    let mut languages: HashMap<Language, usize> = HashMap::new();
    for file in &components[id].own_files {
        if let Some(f) = files.get(file.as_str()) {
            file_count += 1;
            line_count += f.facts.lines;
            *languages.entry(f.language).or_default() += f.facts.lines.max(1);
        }
    }
    for child in children {
        let (f, l, langs) = totals(components, &child, files);
        file_count += f;
        line_count += l;
        for (lang, n) in langs {
            *languages.entry(lang).or_default() += n;
        }
    }
    let component = components.get_mut(id).expect("component exists");
    component.file_count = file_count;
    component.line_count = line_count;
    component.language = languages
        .iter()
        .max_by_key(|(lang, n)| (**n, **lang != Language::Other))
        .map(|(lang, _)| *lang);
    (file_count, line_count, languages)
}

struct Builder<'a> {
    manifests: &'a HashMap<String, Manifest>,
    readmes: &'a HashMap<String, String>,
    files: &'a HashMap<&'a str, &'a SourceFile>,
    components: HashMap<String, Component>,
}

impl Builder<'_> {
    fn is_package(&self, dir: &str) -> bool {
        self.manifests.get(dir).is_some_and(|m| m.is_package)
    }

    fn dir_component(
        &mut self,
        id: String,
        name: String,
        dir: &Dir,
        parent: Option<String>,
        kind: ComponentKind,
    ) -> String {
        let mut own_files = Vec::new();
        let is_package = self.is_package(&id);
        let children = self.children(&id, dir, is_package, &mut own_files);
        let manifest = self.manifests.get(&id);
        let entry_doc = own_files
            .iter()
            .find_map(|f| self.files.get(f.as_str()).and_then(|f| f.facts.doc.clone()));
        let description = manifest
            .and_then(|m| m.description.clone())
            .or_else(|| self.readmes.get(&id).and_then(|r| readme_paragraph(r)))
            .or(entry_doc);
        let interface = own_files
            .iter()
            .filter_map(|f| self.files.get(f.as_str()))
            .flat_map(|f| f.facts.items.iter().cloned())
            .collect();
        let kind = if kind == ComponentKind::Module && is_package {
            ComponentKind::Package
        } else {
            kind
        };
        for child in &children {
            if let Some(c) = self.components.get_mut(child) {
                c.parent = Some(id.clone());
            }
        }
        self.components.insert(
            id.clone(),
            Component {
                id: id.clone(),
                name,
                kind,
                language: None,
                parent,
                children,
                description,
                interface,
                external_deps: manifest.map(|m| m.dependencies.clone()).unwrap_or_default(),
                own_files,
                is_test: is_test_name(file_name(&id)),
                file_count: 0,
                line_count: 0,
            },
        );
        id
    }

    /// The parts of folder `path`, applying the transparency rules of the
    /// module docs. Entry files go to `own_files`.
    fn children(
        &mut self,
        path: &str,
        dir: &Dir,
        is_package: bool,
        own_files: &mut Vec<String>,
    ) -> Vec<String> {
        let mut children = Vec::new();
        for (name, sub) in dir.source_dirs() {
            let sub_path = join(path, name);
            if is_package && name == "src" && !self.is_package(&sub_path) {
                children.extend(self.children(&sub_path, sub, false, own_files));
                continue;
            }
            if !self.is_package(&sub_path) && sub.files.is_empty() {
                let source_dirs: Vec<_> = sub.source_dirs().collect();
                let groups_packages = source_dirs.len() > 1
                    && source_dirs
                        .iter()
                        .all(|(n, _)| self.is_package(&join(&sub_path, n)));
                if groups_packages {
                    for (n, package) in source_dirs {
                        let package_path = join(&sub_path, n);
                        children.push(self.dir_component(
                            package_path,
                            n.clone(),
                            package,
                            Some(path.to_owned()),
                            ComponentKind::Package,
                        ));
                    }
                    continue;
                }
            }
            // Collapse a chain of folders without own files.
            let (mut chain_path, mut chain_name, mut chain_dir) = (sub_path, name.clone(), sub);
            loop {
                if !chain_dir.files.is_empty() || self.is_package(&chain_path) {
                    break;
                }
                let mut source_dirs = chain_dir.source_dirs();
                let (Some((next_name, next)), None) = (source_dirs.next(), source_dirs.next())
                else {
                    break;
                };
                chain_path = join(&chain_path, next_name);
                chain_name = format!("{chain_name}/{next_name}");
                chain_dir = next;
            }
            children.push(self.dir_component(
                chain_path,
                chain_name,
                chain_dir,
                Some(path.to_owned()),
                ComponentKind::Module,
            ));
        }
        for name in &dir.files {
            let file_path = join(path, name);
            if is_entry_file(name) {
                own_files.push(file_path);
                continue;
            }
            // `foo.rs` next to `foo/` is that module's file.
            if let Some(stem) = name.strip_suffix(".rs") {
                let module = join(path, stem);
                if let Some(component) = self.components.get_mut(&module) {
                    component.own_files.push(file_path.clone());
                    if let Some(facts) = self.files.get(file_path.as_str()).map(|f| &f.facts) {
                        if component.description.is_none() {
                            component.description = facts.doc.clone();
                        }
                        component.interface.extend(facts.items.iter().cloned());
                    }
                    continue;
                }
            }
            let facts = self.files.get(file_path.as_str()).map(|f| f.facts.clone());
            self.components.insert(
                file_path.clone(),
                Component {
                    id: file_path.clone(),
                    name: name.clone(),
                    kind: ComponentKind::File,
                    language: None,
                    parent: Some(path.to_owned()),
                    children: Vec::new(),
                    description: facts.as_ref().and_then(|f| f.doc.clone()),
                    interface: facts.map(|f| f.items.clone()).unwrap_or_default(),
                    external_deps: Vec::new(),
                    own_files: vec![file_path.clone()],
                    is_test: is_test_name(name),
                    file_count: 0,
                    line_count: 0,
                },
            );
            children.push(file_path);
        }
        children
    }
}

/// Turns a file's imports into project paths.
struct Resolver {
    files: HashSet<String>,
    dirs: HashSet<String>,
    /// Rust crate identifier → the crate's folder.
    crates: HashMap<String, String>,
    /// Folders of Rust crates, for finding a file's crate.
    crate_dirs: HashSet<String>,
    /// npm package name → its folder.
    npm: HashMap<String, String>,
    npm_dirs: HashSet<String>,
    /// Folders Python's absolute imports start from.
    python_roots: Vec<String>,
}

const SCRIPT_EXTENSIONS: &[&str] = &["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs", "mts"];

impl Resolver {
    fn new(files: &[SourceFile], tree: &Dir, manifests: &HashMap<String, Manifest>) -> Self {
        let mut dirs = HashSet::new();
        fn collect(path: &str, dir: &Dir, out: &mut HashSet<String>) {
            for (name, sub) in &dir.dirs {
                let p = join(path, name);
                collect(&p, sub, out);
                out.insert(p);
            }
        }
        collect("", tree, &mut dirs);
        let files_set: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();

        let mut crates = HashMap::new();
        let mut crate_dirs = HashSet::new();
        let mut npm = HashMap::new();
        let mut npm_dirs = HashSet::new();
        for (dir, manifest) in manifests {
            if !manifest.is_package {
                continue;
            }
            match manifest.kind {
                ManifestKind::Cargo => {
                    crate_dirs.insert(dir.clone());
                    let name = manifest
                        .name
                        .clone()
                        .unwrap_or_else(|| file_name(dir).to_owned());
                    crates.insert(name.replace('-', "_"), dir.clone());
                }
                ManifestKind::Npm => {
                    npm_dirs.insert(dir.clone());
                    if let Some(name) = &manifest.name {
                        npm.insert(name.clone(), dir.clone());
                    }
                }
                ManifestKind::Python => {}
            }
        }

        let mut python_roots: Vec<String> = Vec::new();
        let mut add_root = |root: &str| {
            if !python_roots.iter().any(|r| r == root) {
                python_roots.push(root.to_owned());
            }
        };
        add_root("");
        if dirs.contains("src") {
            add_root("src");
        }
        for file in files.iter().filter(|f| f.language == Language::Python) {
            let dir = parent_dir(&file.path);
            if file_name(&file.path) == "__init__.py" {
                let parent = parent_dir(dir);
                if !dir.is_empty() && !files_set.contains(&join(parent, "__init__.py")) {
                    add_root(parent);
                }
            } else if !files_set.contains(&join(dir, "__init__.py")) {
                add_root(dir);
            }
        }

        Self {
            files: files_set,
            dirs,
            crates,
            crate_dirs,
            npm,
            npm_dirs,
            python_roots,
        }
    }

    fn resolve(&self, file: &SourceFile) -> Vec<String> {
        let mut targets: Vec<String> = file
            .facts
            .imports
            .iter()
            .filter_map(|import| match import {
                Import::RustPath(path) => self.rust(&file.path, path),
                Import::Relative(spec) => self.script_relative(parent_dir(&file.path), spec),
                Import::Package(spec) => self.script_package(&file.path, spec),
                Import::Python { level, module } => self.python(&file.path, *level, module),
            })
            .filter(|target| *target != file.path)
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }

    fn enclosing<'a>(&self, path: &'a str, set: &HashSet<String>) -> Option<&'a str> {
        let mut dir = parent_dir(path);
        loop {
            if set.contains(dir) {
                return Some(dir);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent_dir(dir);
        }
    }

    fn crate_src(&self, crate_dir: &str) -> String {
        let src = join(crate_dir, "src");
        if self.dirs.contains(&src) {
            src
        } else {
            crate_dir.to_owned()
        }
    }

    fn rust(&self, file: &str, path: &[String]) -> Option<String> {
        let first = path.first()?.as_str();
        let module_dir = || {
            if matches!(file_name(file), "lib.rs" | "main.rs" | "mod.rs") {
                parent_dir(file).to_owned()
            } else {
                file.trim_end_matches(".rs").to_owned()
            }
        };
        let (base, rest) = match first {
            "crate" => (
                self.crate_src(self.enclosing(file, &self.crate_dirs)?),
                &path[1..],
            ),
            "self" => (module_dir(), &path[1..]),
            "super" => {
                let mut dir = module_dir();
                let mut n = 0;
                while path.get(n).is_some_and(|s| s == "super") {
                    dir = parent_dir(&dir).to_owned();
                    n += 1;
                }
                (dir, &path[n..])
            }
            ident => (self.crate_src(self.crates.get(ident)?), &path[1..]),
        };
        let mut current = base.clone();
        let mut matched = base;
        for segment in rest {
            let next = join(&current, segment);
            let file = format!("{next}.rs");
            if self.dirs.contains(&next) {
                matched = next.clone();
                current = next;
            } else if self.files.contains(&file) {
                matched = file;
                break;
            } else {
                break;
            }
        }
        Some(matched)
    }

    fn script_file(&self, path: &str) -> Option<String> {
        if self.files.contains(path) {
            return Some(path.to_owned());
        }
        // `./x.js` in TypeScript names `x.ts`.
        let stem = [".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .find_map(|ext| path.strip_suffix(ext))
            .unwrap_or(path);
        for ext in SCRIPT_EXTENSIONS {
            let candidate = format!("{stem}.{ext}");
            if self.files.contains(&candidate) {
                return Some(candidate);
            }
        }
        for ext in SCRIPT_EXTENSIONS {
            let candidate = format!("{path}/index.{ext}");
            if self.files.contains(&candidate) {
                return Some(candidate);
            }
        }
        self.dirs.contains(path).then(|| path.to_owned())
    }

    fn script_relative(&self, dir: &str, spec: &str) -> Option<String> {
        let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        for segment in spec.split('/') {
            match segment {
                "." | "" => {}
                ".." => {
                    parts.pop()?;
                }
                s => parts.push(s),
            }
        }
        self.script_file(&parts.join("/"))
    }

    fn script_package(&self, file: &str, spec: &str) -> Option<String> {
        // Common path aliases (`@/x`, `~/x`) point into the package.
        if let Some(rest) = spec.strip_prefix("@/").or_else(|| spec.strip_prefix("~/")) {
            let base = self.enclosing(file, &self.npm_dirs).unwrap_or("");
            return self
                .script_relative(&join(base, "src"), rest)
                .or_else(|| self.script_relative(base, rest));
        }
        let mut segments = spec.split('/');
        let name = match segments.next()? {
            scope if scope.starts_with('@') => format!("{scope}/{}", segments.next()?),
            name => name.to_owned(),
        };
        self.npm.get(&name).cloned()
    }

    fn python(&self, file: &str, level: usize, module: &[String]) -> Option<String> {
        let walk = |base: &str| -> Option<String> {
            let mut current = base.to_owned();
            let mut matched = None;
            for segment in module {
                let next = join(&current, segment);
                let file = format!("{next}.py");
                if self.dirs.contains(&next) {
                    matched = Some(next.clone());
                    current = next;
                } else if self.files.contains(&file) {
                    matched = Some(file);
                    break;
                } else {
                    break;
                }
            }
            matched
        };
        if level > 0 {
            let mut base = parent_dir(file).to_owned();
            for _ in 1..level {
                base = parent_dir(&base).to_owned();
            }
            return if module.is_empty() {
                Some(base)
            } else {
                walk(&base)
            };
        }
        self.python_roots.iter().find_map(|root| walk(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::extract::extract;

    fn model(files: &[(&str, &str)]) -> ArchitectureModel {
        let mut sources = Vec::new();
        let mut manifests = HashMap::new();
        let mut readmes = HashMap::new();
        for (path, text) in files {
            let name = file_name(path);
            if let Some(manifest) = Manifest::parse(name, text) {
                manifests.insert(parent_dir(path).to_owned(), manifest);
            } else if name.eq_ignore_ascii_case("readme.md") {
                readmes.insert(parent_dir(path).to_owned(), text.to_string());
            } else if let Some(language) = Language::from_path(path) {
                sources.push(SourceFile {
                    path: path.to_string(),
                    language,
                    facts: Arc::new(extract(language, text)),
                });
            }
        }
        build_model(PathBuf::from("/p/demo"), sources, manifests, readmes, false)
    }

    fn children(model: &ArchitectureModel, id: &str) -> Vec<String> {
        let mut children = model.get(id).unwrap().children.clone();
        children.sort();
        children
    }

    fn edges(model: &ArchitectureModel, id: &str) -> Vec<(String, String)> {
        model
            .level_edges(id)
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect()
    }

    #[test]
    fn a_cargo_workspace_shows_its_crates_and_their_dependencies() {
        let model = model(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
            (
                "crates/core/Cargo.toml",
                "[package]\nname = \"demo-core\"\ndescription = \"The domain.\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
            (
                "crates/core/src/lib.rs",
                "//! Core.\npub mod model;\npub use model::Thing;\n",
            ),
            ("crates/core/src/model.rs", "pub struct Thing;\n"),
            (
                "crates/ui/Cargo.toml",
                "[package]\nname = \"ui\"\n\n[dependencies]\ndemo-core = { path = \"../core\" }\n",
            ),
            (
                "crates/ui/src/main.rs",
                "use demo_core::Thing;\nmod view;\n",
            ),
            (
                "crates/ui/src/view/mod.rs",
                "use crate::widgets::Button;\nuse crate::mocks::Fake;\npub fn show() {}\n",
            ),
            ("crates/ui/src/mocks.rs", "pub struct Fake;\n"),
            ("crates/ui/src/view/list.rs", "use super::super::widgets;\n"),
            ("crates/ui/src/widgets.rs", "pub struct Button;\n"),
        ]);
        assert_eq!(children(&model, ""), vec!["crates/core", "crates/ui"]);
        let core = model.get("crates/core").unwrap();
        assert_eq!(core.kind, ComponentKind::Package);
        assert_eq!(core.description.as_deref(), Some("The domain."));
        assert_eq!(core.external_deps, vec!["serde"]);
        assert!(core.interface.contains(&"pub use model::Thing".to_owned()));
        assert_eq!(
            edges(&model, ""),
            vec![("crates/ui".into(), "crates/core".into())]
        );

        // `src` is transparent; `view/` is a module, `main.rs` a file.
        assert_eq!(
            children(&model, "crates/ui"),
            vec![
                "crates/ui/src/main.rs",
                "crates/ui/src/mocks.rs",
                "crates/ui/src/view",
                "crates/ui/src/widgets.rs"
            ]
        );
        assert!(model.get("crates/ui/src/mocks.rs").unwrap().is_test);
        assert_eq!(
            edges(&model, "crates/ui"),
            vec![(
                "crates/ui/src/view".into(),
                "crates/ui/src/widgets.rs".into()
            )]
        );
        assert_eq!(model.uses("crates/ui"), vec!["crates/core"]);
        assert_eq!(model.used_by("crates/core"), vec!["crates/ui"]);
        assert_eq!(model.get("").unwrap().file_count, 7);
    }

    #[test]
    fn a_rust_module_file_belongs_to_its_folder() {
        let model = model(&[
            ("Cargo.toml", "[package]\nname = \"one\"\n"),
            ("src/lib.rs", "pub mod session;\n"),
            (
                "src/session.rs",
                "//! Sessions.\npub mod store;\npub struct Session;\n",
            ),
            ("src/session/store.rs", "pub fn save() {}\n"),
        ]);
        assert_eq!(children(&model, ""), vec!["src/session"]);
        let session = model.get("src/session").unwrap();
        assert_eq!(session.description.as_deref(), Some("Sessions."));
        assert!(session.interface.contains(&"pub struct Session".to_owned()));
        assert_eq!(model.get("").unwrap().name, "one");
    }

    #[test]
    fn typescript_packages_aliases_and_relative_imports() {
        let model = model(&[
            (
                "package.json",
                r#"{"name": "shop", "workspaces": ["packages/*"]}"#,
            ),
            (
                "packages/api/package.json",
                r#"{"name": "@shop/api", "description": "HTTP API"}"#,
            ),
            ("packages/api/src/index.ts", "export * from './routes';\n"),
            (
                "packages/api/src/routes.ts",
                "import { db } from '@shop/db';\n",
            ),
            ("packages/db/package.json", r#"{"name": "@shop/db"}"#),
            ("packages/db/src/index.ts", "export const db = 1;\n"),
            ("packages/web/package.json", r#"{"name": "web"}"#),
            (
                "packages/web/src/app/page.tsx",
                "import { Cart } from '@/components/cart';\n",
            ),
            (
                "packages/web/src/components/cart.tsx",
                "import '@shop/api';\nexport function Cart() {}\n",
            ),
        ]);
        assert_eq!(
            children(&model, ""),
            vec!["packages/api", "packages/db", "packages/web"]
        );
        assert_eq!(
            edges(&model, ""),
            vec![
                ("packages/api".into(), "packages/db".into()),
                ("packages/web".into(), "packages/api".into()),
            ]
        );
        assert_eq!(
            edges(&model, "packages/web"),
            vec![(
                "packages/web/src/app".into(),
                "packages/web/src/components".into()
            )]
        );
    }

    #[test]
    fn python_packages_and_imports() {
        let model = model(&[
            (
                "pyproject.toml",
                "[project]\nname = \"billing\"\ndescription = \"Bills.\"\n",
            ),
            ("src/billing/__init__.py", "\"\"\"Billing.\"\"\"\n"),
            ("src/billing/api/__init__.py", ""),
            (
                "src/billing/api/routes.py",
                "from billing.core import invoices\n",
            ),
            ("src/billing/core/__init__.py", ""),
            ("src/billing/core/invoices.py", "from ..db import session\n"),
            ("src/billing/db.py", "def session():\n    pass\n"),
            ("tests/test_api.py", "import billing.api.routes\n"),
        ]);
        assert_eq!(children(&model, ""), vec!["src/billing", "tests"]);
        assert_eq!(
            edges(&model, ""),
            vec![("tests".into(), "src/billing".into())]
        );
        assert_eq!(
            edges(&model, "src/billing"),
            vec![
                ("src/billing/api".into(), "src/billing/core".into()),
                ("src/billing/core".into(), "src/billing/db.py".into()),
            ]
        );
        assert_eq!(
            model.get("").unwrap().description.as_deref(),
            Some("Bills.")
        );
    }

    #[test]
    fn folder_chains_collapse() {
        let model = model(&[
            ("app/src/main/kotlin/com/acme/App.kt", ""),
            ("app/src/main/kotlin/com/acme/Db.kt", ""),
        ]);
        assert_eq!(children(&model, ""), vec!["app/src/main/kotlin/com/acme"]);
        assert_eq!(
            model.get("app/src/main/kotlin/com/acme").unwrap().name,
            "app/src/main/kotlin/com/acme"
        );
    }

    #[test]
    fn relevant_paths() {
        assert!(is_relevant_path("crates/a/src/lib.rs"));
        assert!(is_relevant_path("Cargo.toml"));
        assert!(is_relevant_path("web/README.md"));
        assert!(!is_relevant_path("target/debug/build.rs"));
        assert!(!is_relevant_path("web/node_modules/x/index.js"));
        assert!(!is_relevant_path("docs/guide.md"));
        assert!(!is_relevant_path("static/app.min.js"));
    }
}
