//! The part of a board file that names the board's identity. The other keys belong to the
//! daemon's board parser and are ignored here, so this reads any board file the daemon accepts.

use serde::Deserialize;

use crate::serial::SerialSource;

/// The format every board file declares.
pub const FORMAT: &str = "lemnos.board";

/// A board file, as far as identity goes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BoardFile {
    pub format: String,
    pub schema_version: u32,
    pub board: BoardSection,
}

/// The `[board]` table: the name, the revision, and where the serial comes from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BoardSection {
    /// The board's family name: lowercase `[a-z0-9-]`, at most 64 bytes, as the fact `family`.
    pub id: String,
    /// The display name, as the fact `model` (with the revision after it).
    #[serde(default)]
    pub name: Option<String>,
    /// The hardware revision, appended to `name` in the fact `model`.
    #[serde(default)]
    pub revision: Option<String>,
    /// Where the serial is read from. Absent, the board has no `serial` fact.
    #[serde(default)]
    pub serial: Option<SerialSource>,
}

/// Parses a board file's identity.
///
/// # Errors
///
/// When the text is not TOML, lacks `[board] id`, or does not declare the `lemnos.board` format.
pub fn parse_board(text: &str) -> Result<BoardFile, String> {
    let file: BoardFile = toml::from_str(text).map_err(|e| e.to_string())?;
    if file.format != FORMAT {
        return Err(format!("format is {:?}, not {FORMAT:?}", file.format));
    }
    Ok(file)
}
