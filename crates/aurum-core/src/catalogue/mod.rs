//! Versioned, validated model catalogue.
//!
//! This is deliberately independent of config parsing: the same strict parser is
//! used for the embedded review file and deployment-owned TOML files.

use crate::error::{Result, UserError};
use language_tags::LanguageTag;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const CATALOGUE_SCHEMA_VERSION: u32 = 1;
const BUILTIN_TOML: &str = include_str!("model-catalogue.v1.toml");

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Stt,
    Tts,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportTier {
    Supported,
    Experimental,
    ExplicitOnly,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "model")]
    pub records: Vec<CatalogueRecord>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default)]
    pub stt: DirectionDefaults,
    #[serde(default)]
    pub tts: DirectionDefaults,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectionDefaults {
    /// The only implicit selection. Language never selects a model: experimental
    /// specialists are reachable only through an explicit model id.
    #[serde(default)]
    pub global: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueRecord {
    pub id: String,
    pub direction: Direction,
    pub provider: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub tier: SupportTier,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub license: String,
    pub origin: Origin,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    DownloadableLocal {
        filename: String,
        url: String,
        size_bytes: u64,
        sha256: String,
    },
    PreparedLocal {
        filename: String,
        size_bytes: u64,
        sha256: String,
        source_url: String,
        revision: String,
        preparation: String,
    },
    TtsPack {
        adapter: String,
        files: Vec<PackFile>,
        voices: Vec<Voice>,
        max_phoneme_tokens: usize,
        sample_rate_hz: u32,
        shipped: bool,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackFile {
    pub filename: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    pub id: String,
    pub internal_key: String,
    pub language: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogueSource {
    Builtin,
    Deployment { path: PathBuf },
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveRecord {
    pub record: CatalogueRecord,
    pub source: CatalogueSource,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct EffectiveCatalogue {
    records: Vec<EffectiveRecord>,
    defaults: Defaults,
}

impl CatalogueDocument {
    pub fn parse(input: &str) -> Result<Self> {
        let doc: Self = toml::from_str(input).map_err(invalid)?;
        doc.validate()?;
        Ok(doc)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|e| UserError::InvalidConfig {
            reason: format!("cannot read catalogue {}: {e}", path.display()),
        })?;
        Self::parse(&text)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CATALOGUE_SCHEMA_VERSION {
            return Err(config_error(format!(
                "catalogue schema_version must be {CATALOGUE_SCHEMA_VERSION}, got {}",
                self.schema_version
            )));
        }
        let mut ids = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        for record in &self.records {
            validate_id(&record.id, "canonical id")?;
            // This is a local model catalogue. Remote providers keep their own
            // reviewed registries and `ProviderCapabilities`; duplicating them
            // here would create a second, drifting source of capability truth.
            if record.provider != "local" {
                return Err(config_error(format!(
                    "catalogue record '{}' must use provider = \"local\"; remote models are \
                     selected with --provider and are not catalogue records",
                    record.id
                )));
            }
            if !ids.insert(record.id.to_ascii_lowercase()) {
                return Err(config_error(format!(
                    "duplicate catalogue id '{}'",
                    record.id
                )));
            }
            for alias in &record.aliases {
                validate_id(alias, "alias")?;
                let key = alias.to_ascii_lowercase();
                if !aliases.insert(key.clone()) || ids.contains(&key) {
                    return Err(config_error(format!(
                        "duplicate or colliding catalogue alias '{alias}'"
                    )));
                }
            }
            for language in &record.languages {
                validate_language(language)?;
            }
            validate_origin(record)?;
        }
        for record in &self.records {
            for alias in &record.aliases {
                if ids.contains(&alias.to_ascii_lowercase()) {
                    return Err(config_error(format!(
                        "alias '{alias}' collides with canonical id"
                    )));
                }
            }
        }
        // Defaults are deliberately not resolved here. A deployment document may
        // point at a built-in record which only exists after the effective view is
        // assembled. Validate the keys now and resolve them after merging.
        self.validate_default_keys()?;
        Ok(())
    }

    fn validate_default_keys(&self) -> Result<()> {
        for defaults in [&self.defaults.stt, &self.defaults.tts] {
            if let Some(id) = &defaults.global {
                validate_id(id, "default model id")?;
            }
        }
        Ok(())
    }
}

impl EffectiveCatalogue {
    pub fn builtin() -> Result<Self> {
        Self::from_documents(builtin_document()?, None)
    }

    pub fn load_deployment(path: &Path) -> Result<Self> {
        Self::from_documents(
            builtin_document()?,
            Some((CatalogueDocument::load(path)?, path.to_path_buf())),
        )
    }

    pub fn from_documents(
        builtin: CatalogueDocument,
        deployment: Option<(CatalogueDocument, PathBuf)>,
    ) -> Result<Self> {
        builtin.validate()?;
        let mut records: BTreeMap<String, EffectiveRecord> = builtin
            .records
            .into_iter()
            .filter(|record| record.enabled)
            .map(|record| {
                let digest = digest(&record);
                (
                    record.id.to_ascii_lowercase(),
                    EffectiveRecord {
                        record,
                        source: CatalogueSource::Builtin,
                        digest,
                    },
                )
            })
            .collect();
        let mut defaults = builtin.defaults;
        if let Some((deployment, path)) = deployment {
            deployment.validate()?;
            // Deployment records replace the entire matching record; nothing is inherited.
            for record in deployment.records {
                let key = record.id.to_ascii_lowercase();
                if record.enabled {
                    let digest = digest(&record);
                    records.insert(
                        key,
                        EffectiveRecord {
                            record,
                            source: CatalogueSource::Deployment { path: path.clone() },
                            digest,
                        },
                    );
                } else if records.remove(&key).is_none() {
                    // Aliases are compatibility names; disabling one must disable
                    // its canonical record rather than silently doing nothing.
                    let alias_key = records.iter().find_map(|(canonical, entry)| {
                        entry
                            .record
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(&record.id))
                            .then(|| canonical.clone())
                    });
                    let Some(alias_key) = alias_key else {
                        return Err(config_error(format!(
                            "disabled model '{}' does not match an effective canonical id or alias",
                            record.id
                        )));
                    };
                    records.remove(&alias_key);
                }
            }
            // An explicit deployment global replaces the built-in one; an absent
            // global retains the built-in default.
            merge_defaults(&mut defaults.stt, deployment.defaults.stt);
            merge_defaults(&mut defaults.tts, deployment.defaults.tts);
        }
        let records: Vec<_> = records.into_values().collect();
        let effective = Self { records, defaults };
        effective.validate_effective()?;
        Ok(effective)
    }

    pub fn records(&self) -> &[EffectiveRecord] {
        &self.records
    }
    /// Digest of the canonical serialized effective records for diagnostics and
    /// resumable batch fingerprints.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(
            // Source paths are diagnostic metadata, not model identity. Moving an
            // identical deployment file must not invalidate resumable batches.
            serde_json::to_vec(
                &self
                    .records
                    .iter()
                    .map(|entry| &entry.record)
                    .collect::<Vec<_>>(),
            )
            .expect("effective catalogue is serializable"),
        ))
    }
    pub fn source_for(&self, id: &str) -> Option<&CatalogueSource> {
        self.lookup(id).map(|r| &r.source)
    }
    pub fn lookup(&self, id: &str) -> Option<&EffectiveRecord> {
        let key = id.trim();
        self.records.iter().find(|entry| {
            entry.record.id.eq_ignore_ascii_case(key)
                || entry
                    .record
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(key))
        })
    }
    /// Resolve a model: CLI id, then configured id, then the global default.
    /// Language is deliberately not an input: it is a decoding hint only and
    /// must never change which weights are selected or downloaded.
    pub fn resolve(
        &self,
        direction: Direction,
        cli: Option<&str>,
        configured: Option<&str>,
    ) -> Result<&EffectiveRecord> {
        if let Some(id) = [cli, configured].into_iter().flatten().next() {
            return self.lookup_direction(id, direction).ok_or_else(|| {
                config_error(format!(
                    "unknown or incompatible {direction:?} model '{id}'"
                ))
            });
        }
        let id = self
            .defaults_for(direction)
            .global
            .as_deref()
            .ok_or_else(|| config_error(format!("no global {direction:?} default")))?;
        self.lookup_direction(id, direction)
            .ok_or_else(|| config_error(format!("default '{id}' is unavailable")))
    }

    fn lookup_direction(&self, id: &str, direction: Direction) -> Option<&EffectiveRecord> {
        self.lookup(id)
            .filter(|entry| entry.record.direction == direction)
    }
    fn defaults_for(&self, direction: Direction) -> &DirectionDefaults {
        match direction {
            Direction::Stt => &self.defaults.stt,
            Direction::Tts => &self.defaults.tts,
        }
    }
    fn validate_effective(&self) -> Result<()> {
        for (direction, defaults) in [
            (Direction::Stt, &self.defaults.stt),
            (Direction::Tts, &self.defaults.tts),
        ] {
            if let Some(id) = &defaults.global {
                validate_effective_default(id, direction, &self.records)?;
            }
        }
        Ok(())
    }
}

/// The embedded v1 document is the complete built-in catalogue. Nothing is
/// synthesized from Rust tables: parity tests instead assert that the legacy
/// `model::MODELS` / `tts::catalogue::MODELS` views match this file exactly.
fn builtin_document() -> Result<CatalogueDocument> {
    let document = CatalogueDocument::parse(BUILTIN_TOML)?;
    #[cfg(not(feature = "tts"))]
    let document = {
        let mut document = document;
        document
            .records
            .retain(|record| record.direction != Direction::Tts);
        document.defaults.tts = DirectionDefaults::default();
        document
    };
    for record in &document.records {
        for url in record_urls(record) {
            reviewed_builtin_url(url)?;
        }
    }
    Ok(document)
}

fn merge_defaults(target: &mut DirectionDefaults, incoming: DirectionDefaults) {
    if incoming.global.is_some() {
        target.global = incoming.global;
    }
}
fn validate_effective_default(
    id: &str,
    direction: Direction,
    records: &[EffectiveRecord],
) -> Result<()> {
    let Some(record) = records.iter().find(|record| {
        record.record.direction == direction
            && record.record.id.eq_ignore_ascii_case(id)
            && record.record.provider == "local"
    }) else {
        return Err(config_error(format!(
            "{direction:?} default '{id}' does not resolve to an enabled compatible record"
        )));
    };
    // Experimental and explicit-only records are reachable by explicit id only;
    // a catalogue (built-in or deployment) can never make one implicit.
    if record.record.tier != SupportTier::Supported {
        return Err(config_error(format!(
            "{direction:?} default '{id}' must be a supported record, not {:?}",
            record.record.tier
        )));
    }
    Ok(())
}
fn validate_id(value: &str, field: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    if valid {
        Ok(())
    } else {
        Err(config_error(format!("invalid {field} '{value}'")))
    }
}
fn validate_language(language: &str) -> Result<()> {
    if language.eq_ignore_ascii_case("auto") {
        return Ok(());
    }
    normalize_language(language).map(|_| ())
}
fn normalize_language(language: &str) -> Result<String> {
    language
        .parse::<LanguageTag>()
        .map(|tag| tag.to_string())
        .map_err(|_| config_error(format!("invalid BCP-47 language tag '{language}'")))
}
fn validate_origin(record: &CatalogueRecord) -> Result<()> {
    match &record.origin {
        Origin::DownloadableLocal {
            filename,
            url,
            size_bytes,
            sha256,
        } => {
            local_record(record)?;
            validate_id(filename, "artifact filename")?;
            safe_https(url)?;
            pin(*size_bytes, sha256)
        }
        Origin::PreparedLocal {
            filename,
            size_bytes,
            sha256,
            source_url,
            revision,
            preparation,
        } => {
            local_record(record)?;
            validate_id(filename, "artifact filename")?;
            safe_https(source_url)?;
            if revision.trim().is_empty() || preparation.trim().is_empty() {
                return Err(config_error(
                    "prepared-local records require immutable revision and preparation guidance",
                ));
            }
            pin(*size_bytes, sha256)
        }
        Origin::TtsPack {
            adapter,
            files,
            voices,
            max_phoneme_tokens,
            sample_rate_hz,
            ..
        } => {
            if record.direction != Direction::Tts || !record.provider.eq_ignore_ascii_case("local")
            {
                return Err(config_error(
                    "tts_pack origins require direction=tts and provider=local",
                ));
            }
            if !matches!(
                adapter.as_str(),
                "kitten-onnx-v1" | "kokoro-onnx-v0" | "placeholder-v0"
            ) || files.is_empty()
                || voices.is_empty()
                || *max_phoneme_tokens == 0
                || *sample_rate_hz == 0
            {
                return Err(config_error(
                    "invalid TTS pack adapter, files, voices, or limits",
                ));
            }
            for file in files {
                validate_id(&file.filename, "pack filename")?;
                safe_https(&file.url)?;
                pin(file.size_bytes, &file.sha256)?;
            }
            for voice in voices {
                validate_id(&voice.id, "voice id")?;
                if voice.internal_key.trim().is_empty() {
                    return Err(config_error("voice internal_key cannot be empty"));
                }
                validate_language(&voice.language)?;
            }
            Ok(())
        }
    }
}
fn record_urls(record: &CatalogueRecord) -> Vec<&str> {
    match &record.origin {
        Origin::DownloadableLocal { url, .. } => vec![url.as_str()],
        Origin::PreparedLocal { source_url, .. } => vec![source_url.as_str()],
        Origin::TtsPack { files, .. } => files.iter().map(|file| file.url.as_str()).collect(),
    }
}
/// Built-in records may only point at the reviewed hosts already used by
/// `model::` / `tts::catalogue`, and Hugging Face URLs must name an immutable
/// 40-hex revision rather than a moving branch such as `main`.
fn reviewed_builtin_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|_| config_error(format!("invalid URL '{url}'")))?;
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    let is_revision = |rev: &str| rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit());
    let reviewed = parsed.scheme() == "https"
        && match parsed.host_str() {
            // huggingface.co/{org}/{repo}/(resolve|tree)/{revision}/...
            Some("huggingface.co") => {
                segments.len() >= 4
                    && matches!(segments[2], "resolve" | "tree")
                    && is_revision(segments[3])
            }
            // github.com/{org}/{repo}/releases/download/{tag}/{asset}
            Some("github.com") => segments.len() == 6 && segments[2..4] == ["releases", "download"],
            _ => false,
        };
    if reviewed {
        Ok(())
    } else {
        Err(config_error(format!(
            "built-in catalogue URL must use a reviewed host and an immutable revision: '{url}'"
        )))
    }
}
fn local_record(record: &CatalogueRecord) -> Result<()> {
    if record.direction == Direction::Stt && record.provider.eq_ignore_ascii_case("local") {
        Ok(())
    } else {
        Err(config_error(
            "local STT artifact origins require direction=stt and provider=local",
        ))
    }
}
fn safe_https(url: &str) -> Result<()> {
    let url = url::Url::parse(url).map_err(|_| config_error(format!("invalid URL '{url}'")))?;
    if url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(())
    } else {
        Err(config_error(format!(
            "origin URL must be safe HTTPS: '{url}'"
        )))
    }
}
fn pin(size: u64, sha256: &str) -> Result<()> {
    if size > 0
        && sha256.len() == 64
        && sha256
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(config_error(
            "integrity pins require nonzero exact size and lowercase 64-hex SHA-256",
        ))
    }
}
fn digest(record: &CatalogueRecord) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(record).expect("catalogue record is serializable"),
    ))
}
fn invalid(error: toml::de::Error) -> crate::error::AurumError {
    config_error(format!("invalid catalogue TOML: {error}"))
}
fn config_error(reason: impl Into<String>) -> crate::error::AurumError {
    UserError::InvalidConfig {
        reason: reason.into(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_catalogue_is_valid() {
        assert!(EffectiveCatalogue::builtin().is_ok());
    }

    #[test]
    fn parser_rejects_unknown_fields_bad_pins_and_invalid_languages() {
        let unknown = "schema_version = 1\nunexpected = true\n";
        assert!(CatalogueDocument::parse(unknown).is_err());
        let invalid = r#"schema_version = 1
[[model]]
id = "bad"
direction = "stt"
provider = "local"
languages = ["not a language"]
tier = "supported"
origin = { kind = "downloadable_local", filename = "bad.bin", url = "https://example.invalid/bad.bin", size_bytes = 1, sha256 = "UPPERCASE" }
"#;
        assert!(CatalogueDocument::parse(invalid).is_err());
    }

    #[test]
    fn toml_stt_records_match_the_legacy_table_exactly() {
        let document = builtin_document().unwrap();
        let stt: Vec<_> = document
            .records
            .iter()
            .filter(|record| record.direction == Direction::Stt)
            .collect();
        // Every legacy name (canonical or alias) is a TOML id or alias with identical pins.
        for model in crate::model::MODELS {
            let record = stt
                .iter()
                .find(|record| {
                    record.id == model.name || record.aliases.iter().any(|a| a == model.name)
                })
                .unwrap_or_else(|| panic!("{} missing from model-catalogue.v1.toml", model.name));
            let (filename, size_bytes, sha256) = match &record.origin {
                Origin::DownloadableLocal {
                    filename,
                    size_bytes,
                    sha256,
                    ..
                }
                | Origin::PreparedLocal {
                    filename,
                    size_bytes,
                    sha256,
                    ..
                } => (filename, *size_bytes, sha256),
                other => panic!("{} has non-local origin {other:?}", model.name),
            };
            assert_eq!(filename, model.filename, "{} filename drift", model.name);
            assert_eq!(
                Some(size_bytes),
                crate::model::pinned_exact_bytes(model.filename),
                "{} exact size drift",
                model.name
            );
            assert_eq!(
                Some(sha256.as_str()),
                crate::model::pinned_sha256(model.filename),
                "{} sha256 drift",
                model.name
            );
            let tier = match crate::model::model_support_tier(model.name) {
                crate::model::ModelSupportTier::Supported => SupportTier::Supported,
                crate::model::ModelSupportTier::Experimental => SupportTier::Experimental,
            };
            assert_eq!(record.tier, tier, "{} tier drift", model.name);
            // The URL a reviewer reads is the URL the downloader fetches.
            let manifest = crate::model::artifact_manifest_json(model);
            match &record.origin {
                Origin::DownloadableLocal { url, .. } => assert_eq!(
                    manifest["download_url_template"],
                    url.as_str(),
                    "{} url drift",
                    model.name
                ),
                Origin::PreparedLocal {
                    revision,
                    source_url,
                    ..
                } => {
                    assert!(manifest["download_url_template"].is_null());
                    assert_eq!(manifest["source_revision"], revision.as_str());
                    assert_eq!(manifest["source_url"], source_url.as_str());
                }
                _ => unreachable!(),
            }
        }
        // And nothing in the TOML is unknown to the legacy download path.
        for record in &stt {
            for name in std::iter::once(&record.id).chain(&record.aliases) {
                assert!(
                    crate::model::lookup_model(name).is_ok(),
                    "{name} is in the TOML but not in model::MODELS"
                );
            }
        }
    }

    #[cfg(feature = "tts")]
    #[test]
    fn toml_tts_records_match_the_legacy_table_exactly() {
        use crate::tts::catalogue as tts;
        let document = builtin_document().unwrap();
        let shipped: Vec<_> = tts::MODELS.iter().filter(|model| model.shipped).collect();
        for model in &shipped {
            let record = document
                .records
                .iter()
                .find(|record| record.id == model.id)
                .unwrap_or_else(|| panic!("{} missing from model-catalogue.v1.toml", model.id));
            assert_eq!(record.direction, Direction::Tts);
            assert_eq!(record.license, model.license);
            assert_eq!(record.languages, model.languages);
            let Origin::TtsPack {
                adapter,
                files,
                voices,
                max_phoneme_tokens,
                sample_rate_hz,
                shipped,
            } = &record.origin
            else {
                panic!("{} is not a tts_pack", model.id);
            };
            assert_eq!(adapter, model.adapter);
            assert_eq!(*max_phoneme_tokens, model.max_phoneme_tokens);
            assert_eq!(*sample_rate_hz, model.sample_rate_hz);
            assert!(*shipped);
            let expected: Vec<_> = [model.onnx, model.voices, model.config]
                .iter()
                .map(|file| {
                    (
                        file.filename.to_string(),
                        tts::pack_file_url(model, file),
                        file.approx_bytes,
                        file.sha256.to_string(),
                    )
                })
                .collect();
            let actual: Vec<_> = files
                .iter()
                .map(|file| {
                    (
                        file.filename.clone(),
                        file.url.clone(),
                        file.size_bytes,
                        file.sha256.clone(),
                    )
                })
                .collect();
            assert_eq!(actual, expected, "{} pack file drift", model.id);
            let expected_voices: Vec<_> = tts::VOICES
                .iter()
                .filter(|voice| voice.model == model.id)
                .map(|voice| (voice.id, voice.internal_key, voice.language))
                .collect();
            let actual_voices: Vec<_> = voices
                .iter()
                .map(|voice| {
                    (
                        voice.id.as_str(),
                        voice.internal_key.as_str(),
                        voice.language.as_str(),
                    )
                })
                .collect();
            assert_eq!(actual_voices, expected_voices, "{} voice drift", model.id);
        }
        let tts_records = document
            .records
            .iter()
            .filter(|record| record.direction == Direction::Tts)
            .count();
        assert_eq!(tts_records, shipped.len(), "TOML has unshipped TTS records");
    }

    #[test]
    fn builtin_urls_use_reviewed_hosts_and_immutable_revisions() {
        // builtin_document() enforces this; assert the policy itself too.
        assert!(builtin_document().is_ok());
        for bad in [
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            "https://example.com/ggml-base.bin",
            "http://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin",
            "https://github.com/org/repo/raw/main/model.onnx",
        ] {
            assert!(reviewed_builtin_url(bad).is_err(), "{bad}");
        }
        assert!(reviewed_builtin_url(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin"
        )
        .is_ok());
    }

    #[cfg(not(feature = "tts"))]
    #[test]
    fn no_tts_build_has_a_valid_stt_only_catalogue() {
        let catalogue = EffectiveCatalogue::builtin().unwrap();
        assert!(catalogue
            .records()
            .iter()
            .all(|record| { record.record.direction == Direction::Stt }));
        assert_eq!(
            catalogue
                .resolve(Direction::Stt, None, None)
                .unwrap()
                .record
                .id,
            "base"
        );
    }
    #[test]
    fn resolver_prefers_cli_then_configured_then_global() {
        let catalogue = EffectiveCatalogue::builtin().unwrap();
        assert_eq!(
            catalogue
                .resolve(Direction::Stt, Some("tiny"), Some("small"))
                .unwrap()
                .record
                .id,
            "tiny"
        );
        assert_eq!(
            catalogue
                .resolve(Direction::Stt, None, Some("small"))
                .unwrap()
                .record
                .id,
            "small"
        );
        assert_eq!(
            catalogue
                .resolve(Direction::Stt, None, None)
                .unwrap()
                .record
                .id,
            "base"
        );
        // Experimental specialists stay reachable by explicit id.
        assert_eq!(
            catalogue
                .resolve(Direction::Stt, None, Some("medium-ptbr-q5_0"))
                .unwrap()
                .record
                .id,
            "medium-ptbr-q5_0"
        );
    }

    #[test]
    fn builtin_defaults_are_supported_local_records() {
        let catalogue = EffectiveCatalogue::builtin().unwrap();
        let stt = catalogue.resolve(Direction::Stt, None, None).unwrap();
        assert_eq!(stt.record.id, "base");
        assert_eq!(stt.record.tier, SupportTier::Supported);
    }

    #[test]
    fn language_defaults_are_rejected_by_the_schema() {
        let deployment = "schema_version = 1\n[defaults.stt.language]\npt-BR = \"tiny\"\n";
        assert!(CatalogueDocument::parse(deployment).is_err());
    }

    #[test]
    fn experimental_records_cannot_be_defaults() {
        for id in ["medium-ptbr-q5_0", "large-v3-ptpt-q5_0", "large-v3-q5_0"] {
            let deployment = CatalogueDocument::parse(&format!(
                "schema_version = 1\n[defaults.stt]\nglobal = \"{id}\"\n"
            ))
            .unwrap();
            let err = EffectiveCatalogue::from_documents(
                builtin_document().unwrap(),
                Some((deployment, PathBuf::from("/tmp/deploy.toml"))),
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("must be a supported record"), "{id}: {err}");
        }
        let explicit_only = CatalogueDocument::parse(r#"schema_version = 1
[defaults.stt]
global = "pinned-only"
[[model]]
id = "pinned-only"
direction = "stt"
provider = "local"
tier = "explicit_only"
origin = { kind = "downloadable_local", filename = "pinned-only.bin", url = "https://example.invalid/pinned-only.bin", size_bytes = 1, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        assert!(EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((explicit_only, PathBuf::from("/tmp/deploy.toml"))),
        )
        .is_err());
    }
    #[test]
    fn deployment_replaces_and_disable_removes_aliases() {
        let base = builtin_document().unwrap();
        let deploy = CatalogueDocument::parse(r#"schema_version = 1
[[model]]
id = "tiny"
direction = "stt"
provider = "local"
enabled = false
tier = "supported"
origin = { kind = "downloadable_local", filename = "tiny.bin", url = "https://example.com/tiny.bin", size_bytes = 1, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let effective = EffectiveCatalogue::from_documents(
            base,
            Some((deploy, PathBuf::from("/tmp/deploy.toml"))),
        )
        .unwrap();
        assert!(effective.lookup("tiny").is_none());
    }

    #[test]
    fn deployment_replacement_has_no_inherited_aliases_or_metadata() {
        let deployment_path = PathBuf::from("/tmp/deployment-catalogue.toml");
        // `base` becomes experimental here, so the default must move off it.
        let deployment = CatalogueDocument::parse(r#"schema_version = 1
[defaults.stt]
global = "tiny"
[[model]]
id = "base"
aliases = ["replacement-base"]
direction = "stt"
provider = "local"
tier = "experimental"
notes = "deployment-owned record"
license = "Apache-2.0"
origin = { kind = "downloadable_local", filename = "replacement-base.bin", url = "https://example.invalid/replacement-base.bin", size_bytes = 42, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let effective = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, deployment_path.clone())),
        )
        .unwrap();
        let base = effective.lookup("base").unwrap();
        assert_eq!(base.record.notes, "deployment-owned record");
        assert_eq!(base.record.aliases, ["replacement-base"]);
        assert!(
            matches!(base.source, CatalogueSource::Deployment { ref path } if path == &deployment_path)
        );

        // A replaced record does not inherit the built-in aliases (`large`).
        let deployment = CatalogueDocument::parse(r#"schema_version = 1
[[model]]
id = "large-v3"
direction = "stt"
provider = "local"
tier = "supported"
origin = { kind = "downloadable_local", filename = "replacement-large.bin", url = "https://example.invalid/replacement-large.bin", size_bytes = 42, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let builtin = EffectiveCatalogue::builtin().unwrap();
        assert_eq!(builtin.lookup("large").unwrap().record.id, "large-v3");
        let effective = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, deployment_path)),
        )
        .unwrap();
        assert!(effective.lookup("large-v3").is_some());
        assert!(effective.lookup("large").is_none());
    }

    #[test]
    fn effective_digest_changes_when_a_record_changes() {
        let builtin = EffectiveCatalogue::builtin().unwrap();
        let deployment = CatalogueDocument::parse(r#"schema_version = 1
[[model]]
id = "base"
direction = "stt"
provider = "local"
tier = "supported"
notes = "changed effective record"
origin = { kind = "downloadable_local", filename = "changed-base.bin", url = "https://example.invalid/changed-base.bin", size_bytes = 42, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let changed = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, PathBuf::from("/tmp/changed.toml"))),
        )
        .unwrap();
        assert_ne!(builtin.digest(), changed.digest());
    }

    #[test]
    fn deployment_default_can_target_builtin_record() {
        let deployment =
            CatalogueDocument::parse("schema_version = 1\n[defaults.stt]\nglobal = \"tiny\"\n")
                .unwrap();
        let effective = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, PathBuf::from("/tmp/deploy.toml"))),
        )
        .unwrap();
        assert_eq!(
            effective
                .resolve(Direction::Stt, None, None)
                .unwrap()
                .record
                .id,
            "tiny"
        );
    }

    #[test]
    fn remote_records_are_not_catalogue_records() {
        // The old remote origin kind no longer exists in the schema.
        let remote_origin = r#"schema_version = 1
[[model]]
id = "remote"
direction = "stt"
provider = "openai"
tier = "supported"
origin = { kind = "remote", wire_model = "whisper-1", capabilities = { timestamps_reliable = true } }
"#;
        assert!(CatalogueDocument::parse(remote_origin).is_err());
        // A non-local provider is rejected even with a local-looking origin, so
        // a deployment can never make a remote provider implicit.
        let remote_provider = r#"schema_version = 1
[defaults.stt]
global = "remote"
[[model]]
id = "remote"
direction = "stt"
provider = "openai"
tier = "supported"
origin = { kind = "downloadable_local", filename = "remote.bin", url = "https://example.invalid/remote.bin", size_bytes = 1, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#;
        let err = CatalogueDocument::parse(remote_provider)
            .unwrap_err()
            .to_string();
        assert!(err.contains("provider = \"local\""), "{err}");
        // And the built-in catalogue carries no remote rows at all.
        assert!(builtin_document()
            .unwrap()
            .records
            .iter()
            .all(|record| record.provider == "local"));
    }

    #[test]
    fn alias_disable_removes_the_canonical_record() {
        let deployment = CatalogueDocument::parse(r#"schema_version = 1
[[model]]
id = "turbo"
direction = "stt"
provider = "local"
enabled = false
tier = "supported"
origin = { kind = "downloadable_local", filename = "unused.bin", url = "https://example.invalid/unused.bin", size_bytes = 1, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let effective = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, PathBuf::from("/tmp/deploy.toml"))),
        )
        .unwrap();
        assert!(effective.lookup("large-v3-turbo").is_none());
        assert!(effective.lookup("turbo").is_none());
    }

    #[test]
    fn digest_does_not_depend_on_deployment_path() {
        let deployment = CatalogueDocument::parse(r#"schema_version = 1
[[model]]
id = "base"
direction = "stt"
provider = "local"
tier = "supported"
notes = "same content"
origin = { kind = "downloadable_local", filename = "base.bin", url = "https://example.invalid/base.bin", size_bytes = 42, sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
"#).unwrap();
        let a = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment.clone(), PathBuf::from("/tmp/a.toml"))),
        )
        .unwrap();
        let b = EffectiveCatalogue::from_documents(
            builtin_document().unwrap(),
            Some((deployment, PathBuf::from("/elsewhere/b.toml"))),
        )
        .unwrap();
        assert_eq!(a.digest(), b.digest());
    }
}
