use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Install a system Hangul font as a fallback so Korean glyphs render.
/// egui's bundled Ubuntu/Hack fonts have no Hangul coverage.
pub fn install_korean_fonts(ctx: &egui::Context) {
    let Some((path, bytes)) = load_hangul_font() else {
        tracing::warn!("no Hangul system font found; Korean UI text will be missing glyphs");
        return;
    };

    let mut data = FontData::from_owned(bytes);
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttc"))
    {
        data.index = 0;
    }

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("hangul".to_owned(), Arc::new(data));

    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.push("hangul".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
        family.push("hangul".to_owned());
    }

    ctx.set_fonts(fonts);
    tracing::info!("loaded Hangul UI font from {}", path.display());
}

fn load_hangul_font() -> Option<(PathBuf, Vec<u8>)> {
    for path in candidate_paths() {
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                return Some((path, bytes));
            }
        }
    }
    None
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(fonts_dir) = windows_fonts_dir() {
        for name in ["malgun.ttf", "malgunsl.ttf", "malgunbd.ttf", "gulim.ttc"] {
            paths.push(fonts_dir.join(name));
        }
    }

    paths.extend(
        [
            r"C:\Windows\Fonts\malgun.ttf",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
            "/Library/Fonts/AppleGothic.ttf",
            "/Library/Fonts/NanumGothic.ttf",
            "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansKR-Regular.otf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ]
        .into_iter()
        .map(PathBuf::from),
    );

    paths
}

fn windows_fonts_dir() -> Option<PathBuf> {
    let windir = std::env::var_os("WINDIR")?;
    let dir = Path::new(&windir).join("Fonts");
    dir.is_dir().then_some(dir)
}
