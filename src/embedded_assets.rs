use rust_embed::Embed;

#[derive(Embed)]
#[folder = "prompts/"]
pub struct AssetPrompts;

#[derive(Embed)]
#[folder = "utils/"]
#[include = "rmpp/uinput-*"]
pub struct AssetUtils;

#[derive(Embed)]
#[folder = "assets/fonts/"]
pub struct AssetFonts;

// Function to provide access to the uinput module data
pub fn get_uinput_module_data(version: &str) -> Option<Vec<u8>> {
    let target_module_filename = format!("rmpp/uinput-{}.ko", version);
    AssetUtils::get(target_module_filename.as_str()).map(|asset| asset.data.to_vec())
}

/// The bundled fonts used for drawn answers, so rendering doesn't depend on
/// whatever fonts happen to be installed on the device: a handwriting-style
/// font for Latin text, and a plain, clear sans font for Chinese (legibility
/// over style, since a stylized/cursive Chinese font reads poorly).
pub fn get_answer_font_data() -> Vec<Vec<u8>> {
    ["PatrickHand-Regular.ttf", "NotoSansSC-Regular.ttf"]
        .iter()
        .map(|name| AssetFonts::get(name).unwrap_or_else(|| panic!("bundled {name} font asset is missing")).data.to_vec())
        .collect()
}

pub fn load_config(filename: &str) -> String {
    log::debug!("Loading config from {}", filename);

    if std::path::Path::new(filename).exists() {
        std::fs::read_to_string(filename).unwrap()
    } else {
        std::str::from_utf8(AssetPrompts::get(filename).unwrap().data.as_ref()).unwrap().to_string()
    }
}
