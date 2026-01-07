//! Shared option parsing helpers.

use swc_ecma_ast::EsVersion;

use crate::deobfuscator::SourceType;

/// Map numeric ECMAScript version to SWC enum.
#[must_use]
pub const fn map_es_version_num(value: u32) -> Option<EsVersion> {
    match value {
        3 => Some(EsVersion::Es3),
        5 => Some(EsVersion::Es5),
        6 | 2015 => Some(EsVersion::Es2015),
        7 | 2016 => Some(EsVersion::Es2016),
        8 | 2017 => Some(EsVersion::Es2017),
        9 | 2018 => Some(EsVersion::Es2018),
        10 | 2019 => Some(EsVersion::Es2019),
        11 | 2020 => Some(EsVersion::Es2020),
        12 | 2021 => Some(EsVersion::Es2021),
        13 | 2022 => Some(EsVersion::Es2022),
        2023 => Some(EsVersion::Es2023),
        2024 => Some(EsVersion::Es2024),
        _ => None,
    }
}

/// Parse ECMAScript version string (e.g. "es2020", "2020", "latest").
///
/// # Errors
///
/// Returns an error if the version string is empty or not recognized.
pub fn parse_es_version_str(raw: &str) -> Result<EsVersion, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("ECMAScript version is empty".to_owned());
    }

    let lower = trimmed.to_lowercase();
    let normalized = lower.strip_prefix("es").unwrap_or(&lower);

    if normalized == "next" || normalized == "latest" {
        return Ok(EsVersion::EsNext);
    }

    if let Ok(num) = normalized.parse::<u32>() {
        return map_es_version_num(num)
            .ok_or_else(|| format!("Unknown ECMAScript version: {trimmed}"));
    }

    Err(format!("Unknown ECMAScript version: {trimmed}"))
}

/// Parse source type string ("script", "module", or "both").
///
/// # Errors
///
/// Returns an error if the source type is not recognized.
pub fn parse_source_type_str(raw: &str) -> Result<SourceType, String> {
    match raw.trim().to_lowercase().as_str() {
        "module" => Ok(SourceType::Module),
        "script" => Ok(SourceType::Script),
        "both" => Ok(SourceType::Both),
        other => Err(format!("Unknown source type: {other}")),
    }
}
