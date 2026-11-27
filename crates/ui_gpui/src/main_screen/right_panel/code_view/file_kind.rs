//! What a changed file is, by its path alone: a test (the "hide tests"
//! toggle) and which tier it is listed in (sources, then tests, then
//! generated files). Ported from T3 Code's `classifyChangedFile` and
//! `diffFileTier`; T3 keeps the two apart and so do we: the toggle hides
//! test support (mocks, fixtures) too, the order only knows the three tiers.

/// The tier a file is listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Source,
    Test,
    Generated,
}

const GENERATED_FILES: &[&str] = &[
    "bun.lock",
    "bun.lockb",
    "cargo.lock",
    "composer.lock",
    "deno.lock",
    "flake.lock",
    "gemfile.lock",
    "go.sum",
    "gradle.lockfile",
    "mix.lock",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "packages.lock.json",
    "pipfile.lock",
    "pnpm-lock.yaml",
    "podfile.lock",
    "poetry.lock",
    "pubspec.lock",
    "uv.lock",
    "yarn.lock",
];
const GENERATED_DIRS: &[&str] = &["__generated__", "generated"];
const MOCK_WORDS: &[&str] = &[
    "mock", "mocks", "fixture", "fixtures", "stub", "stubs", "fake", "fakes",
];
const MOCK_DIRS: &[&str] = &[
    "__fixtures__",
    "__mocks__",
    "__snapshots__",
    "fakes",
    "fixture",
    "fixtures",
    "mock",
    "mocks",
    "stubs",
    "testdata",
];
const TEST_DIRS: &[&str] = &[
    "__tests__",
    "__test__",
    "cypress",
    "e2e",
    "spec",
    "specs",
    "test",
    "tests",
];
const CLASS_TEST_EXTENSIONS: &[&str] = &[
    "java", "kt", "kts", "scala", "groovy", "cs", "php", "swift", "m",
];

/// Whether the "hide tests" toggle hides `path`: tests and what only
/// tests use (mocks, fixtures, snapshots).
pub fn is_test_file(path: &str) -> bool {
    let (dirs, name) = split_path(path);
    let dirs: Vec<String> = dirs.iter().map(|d| d.to_lowercase()).collect();
    let in_dir = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    if is_generated_name(name) || in_dir(GENERATED_DIRS) {
        return false;
    }
    is_mock_name(name) || in_dir(MOCK_DIRS) || is_test_name(name) || in_dir(TEST_DIRS)
}

/// The tier `path` is listed in.
pub fn tier(path: &str) -> Tier {
    const NAMES: &[&str] = &[
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
        "bun.lockb",
        "Cargo.lock",
        "go.sum",
        "composer.lock",
        "Gemfile.lock",
    ];
    const DIRS: &[&str] = &["__snapshots__", "__generated__", "dist", "build", "vendor"];
    const TEST_DIRS: &[&str] = &["__tests__", "tests", "test"];
    let (dirs, name) = split_path(path);
    if NAMES.contains(&name)
        || [".snap", ".min.js", ".min.css"]
            .iter()
            .any(|end| name.ends_with(end))
        || name.contains(".generated.")
        || dirs.iter().any(|d| DIRS.contains(d))
    {
        Tier::Generated
    } else if name.contains(".test.")
        || name.contains(".spec.")
        || dirs.iter().any(|d| TEST_DIRS.contains(d))
    {
        Tier::Test
    } else {
        Tier::Source
    }
}

/// The directories and the file name of `path` (`\` counts as `/`).
fn split_path(path: &str) -> (Vec<&str>, &str) {
    let mut segments: Vec<&str> = path.split(['/', '\\']).collect();
    let name = segments.pop().unwrap_or_default();
    (segments, name)
}

/// `X` of a name ending in `.X.ext`.
fn inner_extension(name: &str) -> Option<&str> {
    let mut parts = name.rsplitn(3, '.');
    let ext = parts.next()?;
    let inner = parts.next()?;
    (parts.next().is_some() && !ext.is_empty()).then_some(inner)
}

fn is_generated_name(name: &str) -> bool {
    GENERATED_FILES.contains(&name.to_lowercase().as_str())
        || name.contains(".generated.")
        || name.contains(".gen.")
        || [
            ".g.dart",
            ".pb.go",
            "_pb2.py",
            "_pb2_grpc.py",
            ".pb.cc",
            ".pb.h",
            ".min.js",
            ".min.css",
            ".js.map",
            ".css.map",
        ]
        .iter()
        .any(|end| name.ends_with(end))
}

/// Mocks, fixtures, stubs, fakes (by an inner extension or a leading word)
/// and snapshots.
fn is_mock_name(name: &str) -> bool {
    if name.ends_with(".snap") || inner_extension(name).is_some_and(|x| MOCK_WORDS.contains(&x)) {
        return true;
    }
    MOCK_WORDS.iter().any(|word| {
        name.strip_prefix(word).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| matches!(c, '-' | '_' | '.') || c.is_ascii_uppercase())
        })
    })
}

fn is_test_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let stem_ends = |suffix: &str| {
        lower
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !ext.is_empty() && stem.ends_with(suffix))
    };
    inner_extension(&lower).is_some_and(|x| matches!(x, "test" | "spec" | "e2e"))
        || stem_ends("_test")
        || stem_ends("_spec")
        || (lower.starts_with("test_") && lower.ends_with(".py") && lower.len() > 8)
        || lower == "conftest.py"
        || ["vitest.setup.", "jest.setup."]
            .iter()
            .any(|p| lower.strip_prefix(p).is_some_and(|ext| !ext.is_empty() && !ext.contains('.')))
        // Case-sensitive: `FooTest.java` is a test class, `Latest.java` is not.
        || name.rsplit_once('.').is_some_and(|(stem, ext)| {
            CLASS_TEST_EXTENSIONS.contains(&ext)
                && ["Test", "Tests", "Spec"].iter().any(|s| stem.ends_with(s))
        })
        // A Rust module of tests (`mod tests;`).
        || name == "tests.rs"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_and_test_support_are_test_files() {
        for path in [
            "src/a.test.ts",
            "src/a.spec.tsx",
            "e2e/login.e2e.ts",
            "pkg/a_test.go",
            "lib/a_spec.rb",
            "tests/test_api.py",
            "conftest.py",
            "vitest.setup.ts",
            "src/FooTest.java",
            "src/FooTests.cs",
            "crates/a/tests/it.rs",
            "crates/a/src/session/tests.rs",
            "src/__tests__/a.ts",
            "test/fixtures/pr.json",
            "src/__mocks__/fs.ts",
            "src/api.mock.ts",
            "src/mockServer.ts",
            "src/fixtures-data.json",
            "src/__snapshots__/a.ts.snap",
            "testdata/input.txt",
            "tests/README.md",
        ] {
            assert!(is_test_file(path), "{path} is a test file");
        }
    }

    #[test]
    fn code_docs_and_generated_files_are_not() {
        for path in [
            "src/contest.py",
            "src/Latest.java",
            "src/testing.ts",
            "src/mockingbird.ts",
            "README.md",
            "pnpm-lock.yaml",
            "Cargo.lock",
            "src/api.generated.ts",
            "tests/generated/a.ts",
            "public/app.min.js",
            "src/a.rs",
        ] {
            assert!(!is_test_file(path), "{path} is no test file");
        }
    }

    #[test]
    fn tiers_follow_the_path_conventions() {
        for path in ["src/app.ts", "src/dist.ts", "src/testing.ts"] {
            assert_eq!(tier(path), Tier::Source, "{path}");
        }
        for path in [
            "src/__tests__/app.ts",
            "src/app.spec.tsx",
            "src/app.test.ts",
            "test/app.ts",
            "tests/helpers/app.ts",
        ] {
            assert_eq!(tier(path), Tier::Test, "{path}");
        }
        for path in [
            "apps/web/package-lock.json",
            "dist/app.js",
            "packages/core/vendor/lib.js",
            "pnpm-lock.yaml",
            "public/app.min.js",
            "src/__snapshots__/app.ts",
            "src/api.generated.ts",
            "src/app.test.ts.snap",
            "Cargo.lock",
        ] {
            assert_eq!(tier(path), Tier::Generated, "{path}");
        }
    }
}
