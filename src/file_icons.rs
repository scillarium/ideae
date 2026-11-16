//! File and folder icons as JetBrains' new UI draws them (assets/file-icons,
//! most of them theirs: see the NOTICE there). They have their own colors,
//! and a second drawing for dark themes where the light one would not show.

use std::{cell::RefCell, collections::HashMap, sync::Arc};

use gpui_kit::component::Theme;
use gpui_kit::*;

macro_rules! icons {
    (themed: [$($themed:literal),* $(,)?], plain: [$($plain:literal),* $(,)?]) => {
        fn svg(name: &str, dark: bool) -> &'static [u8] {
            match (name, dark) {
                $(
                    ($themed, false) => include_bytes!(concat!("../assets/file-icons/", $themed, ".svg")),
                    ($themed, true) => include_bytes!(concat!("../assets/file-icons/", $themed, "_dark.svg")),
                )*
                $(($plain, _) => include_bytes!(concat!("../assets/file-icons/", $plain, ".svg")),)*
                _ => svg("text", dark),
            }
        }
    };
}

icons! {
    themed: [
        "archive", "binaryData", "c", "config", "cpp", "Csharp", "css", "csv", "docker", "editorConfig", "folder",
        "folderGithub", "font", "go", "gradle", "graphql", "groovy", "h", "html", "image", "java", "javaScript",
        "json", "jupyter", "kotlin", "markdown", "patch", "perl", "properties", "python", "rst",
        "rust", "shell", "sql", "swiftLang", "terraform", "text", "toml", "typeScript", "xml", "yaml",
    ],
    plain: ["gitignore", "http", "php", "vue"]
}

pub fn folder(name: &str) -> &'static str {
    if name == ".github" { "folderGithub" } else { "folder" }
}

pub fn file(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "dockerfile" | "containerfile" | ".dockerignore" => return "docker",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".ignore" => return "gitignore",
        ".editorconfig" => return "editorConfig",
        "makefile" | "justfile" | ".env" => return "config",
        "build.gradle" | "settings.gradle" | "build.gradle.kts" | "settings.gradle.kts" => return "gradle",
        _ => {}
    }
    if lower.starts_with("dockerfile.") {
        return "docker";
    }
    if lower.starts_with(".env.") {
        return "config";
    }
    let Some((_, ext)) = lower.rsplit_once('.') else { return "text" };
    match ext {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javaScript",
        "ts" | "mts" | "cts" | "tsx" => "typeScript",
        "vue" => "vue",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "groovy" => "groovy",
        "gradle" => "gradle",
        "swift" => "swiftLang",
        "c" => "c",
        "h" | "hpp" | "hh" | "hxx" => "h",
        "cc" | "cpp" | "cxx" | "c++" | "m" | "mm" => "cpp",
        "cs" => "Csharp",
        "php" | "phtml" => "php",
        "pl" | "pm" => "perl",
        "json" | "jsonc" | "json5" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "xml" | "xsd" | "xsl" | "plist" | "twig" => "xml",
        "html" | "htm" | "xhtml" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "md" | "markdown" | "mdx" => "markdown",
        "rst" => "rst",
        "csv" | "tsv" => "csv",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "tf" | "tfvars" | "hcl" => "terraform",
        "ipynb" => "jupyter",
        "http" | "rest" => "http",
        "properties" => "properties",
        "ini" | "cfg" | "conf" | "env" | "lock" => "config",
        "sh" | "bash" | "zsh" | "fish" | "nu" => "shell",
        "bat" | "cmd" | "ps1" | "psm1" => "shell",
        "patch" | "diff" => "patch",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "svg" | "tiff" | "avif" => "image",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "war" => "archive",
        "exe" | "dll" | "so" | "dylib" | "a" | "o" | "bin" | "wasm" | "class" | "pyc" => "binaryData",
        _ => "text",
    }
}

/// Names already seen, and their file icons: the tree asks on every frame.
const NAMES_KEPT: usize = 4096;

/// `file` of a name, kept from the last time it was asked.
pub fn file_kept(name: &SharedString) -> &'static str {
    NAMES.with_borrow_mut(|names| {
        if let Some(icon) = names.get(name) {
            return *icon;
        }
        if names.len() >= NAMES_KEPT {
            names.clear();
        }
        *names.entry(name.clone()).or_insert_with(|| file(name))
    })
}

thread_local! {
    static NAMES: RefCell<HashMap<SharedString, &'static str>> = RefCell::new(HashMap::new());
    static IMAGES: RefCell<HashMap<(&'static str, bool), Arc<Image>>> = RefCell::new(HashMap::new());
}

/// The icon by its name from `file` or `folder`, for the current theme.
pub fn icon(name: &'static str, cx: &App) -> impl IntoElement {
    let dark = Theme::global(cx).mode.is_dark();
    let image = IMAGES.with_borrow_mut(|images| {
        images.entry((name, dark)).or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Svg, svg(name, dark).to_vec()))).clone()
    });
    img(image).size_4().flex_none()
}

#[cfg(test)]
mod tests {
    use super::{file, folder, svg};

    #[test]
    fn names_find_their_icons() {
        assert_eq!(file("main.rs"), "rust");
        assert_eq!(file("Cargo.lock"), "config");
        assert_eq!(file("Dockerfile"), "docker");
        assert_eq!(file("README.MD"), "markdown");
        assert_eq!(file("LICENSE"), "text");
        assert_eq!(folder(".github"), "folderGithub");
        for name in ["rust", "php", "gitignore", "folder"] {
            for dark in [false, true] {
                assert!(std::str::from_utf8(svg(name, dark)).unwrap().contains("<svg"));
            }
        }
    }
}
