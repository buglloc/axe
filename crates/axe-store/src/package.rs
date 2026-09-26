use std::collections::{BTreeMap, BTreeSet};

use axe_artifact::{Target, validate_relative_path};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDefinition {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub synopsis: Option<String>,
    pub channels: BTreeMap<String, String>,
    pub targets: BTreeSet<Target>,
    pub artifact: ArtifactSpec,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactSpec {
    SingleBinary { path: String },
    Package { entrypoint: String },
}

impl PackageDefinition {
    pub fn exact_version(&self) -> Result<&str, String> {
        let mut versions = self.channels.values();
        let Some(version) = versions.next() else {
            return Err(format!("package {} has no channels", self.id));
        };

        if version == "latest" || versions.any(|candidate| candidate != version) {
            return Err(format!(
                "package {} channels must all name one exact version",
                self.id
            ));
        }
        validate_name(version, "version")?;
        Ok(version)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_path(&self.id, "package ID")?;
        validate_name(&self.name, "package name")?;
        self.exact_version()?;

        let mut names = BTreeSet::new();
        names.insert(self.name.as_str());
        for alias in &self.aliases {
            validate_name(alias, "alias")?;
            if !names.insert(alias) {
                return Err(format!(
                    "package {} repeats name or alias {alias:?}",
                    self.id
                ));
            }
        }

        for channel in self.channels.keys() {
            validate_name(channel, "channel")?;
        }

        if self.targets.is_empty() {
            return Err(format!("package {} has no targets", self.id));
        }

        match &self.artifact {
            ArtifactSpec::SingleBinary { path } => validate_path(path, "binary path")?,
            ArtifactSpec::Package { entrypoint } => {
                validate_path(entrypoint, "package entrypoint")?
            }
        }
        Ok(())
    }
}

pub fn parse_package_set(
    bytes: &[u8],
    selected: Option<&str>,
) -> Result<Vec<(String, PackageDefinition)>, String> {
    let definitions: BTreeMap<String, PackageDefinition> = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse Nix package metadata: {error}"))?;
    let mut packages = Vec::new();
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();

    for (attribute, package) in definitions {
        validate_attribute(&attribute)?;
        package
            .validate()
            .map_err(|error| format!("Nix package {attribute}: {error}"))?;
        if !ids.insert(package.id.clone()) {
            return Err(format!("duplicate package ID {}", package.id));
        }
        for name in std::iter::once(&package.name).chain(&package.aliases) {
            if !names.insert(name.clone()) {
                return Err(format!("duplicate AXE Store name or alias {name}"));
            }
        }

        if selected.is_some_and(|selected| {
            selected != attribute
                && selected != package.name
                && selected != package.id
                && !package.aliases.iter().any(|alias| alias == selected)
        }) {
            continue;
        }
        packages.push((attribute, package));
    }

    if let Some(selected) = selected
        && packages.is_empty()
    {
        return Err(format!("package {selected:?} was not found"));
    }
    Ok(packages)
}

fn validate_attribute(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid Nix package attribute {value:?}"));
    }
    Ok(())
}

fn validate_name(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!("invalid {label} {value:?}"));
    }
    Ok(())
}

fn validate_path(value: &str, label: &str) -> Result<(), String> {
    validate_relative_path(value, label).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_divergent_channels_and_unknown_fields() {
        for metadata in [
            br#"{
                "foo": {
                    "id": "cli/foo",
                    "name": "foo",
                    "aliases": [],
                    "synopsis": null,
                    "channels": {"stable": "1.2.3", "beta": "1.2.4"},
                    "targets": ["x86_64-linux"],
                    "artifact": {"type": "single_binary", "path": "bin/foo"}
                }
            }"#
            .as_slice(),
            br#"{
                "foo": {
                    "id": "cli/foo",
                    "name": "foo",
                    "aliases": [],
                    "synopsis": null,
                    "channels": {"stable": "1.2.3"},
                    "targets": ["x86_64-linux"],
                    "artifact": {"type": "single_binary", "path": "bin/foo"},
                    "source": "untrusted-extension"
                }
            }"#
            .as_slice(),
        ] {
            assert!(parse_package_set(metadata, None).is_err());
        }
    }

    #[test]
    fn rejects_duplicate_store_names() {
        let error = parse_package_set(
            br#"{
                "one": {
                    "id": "cli/one",
                    "name": "duplicate",
                    "aliases": [],
                    "synopsis": null,
                    "channels": {"stable": "1.2.3"},
                    "targets": ["x86_64-linux"],
                    "artifact": {"type": "single_binary", "path": "bin/one"}
                },
                "two": {
                    "id": "cli/two",
                    "name": "duplicate",
                    "aliases": [],
                    "synopsis": null,
                    "channels": {"stable": "1.2.3"},
                    "targets": ["x86_64-linux"],
                    "artifact": {"type": "single_binary", "path": "bin/two"}
                }
            }"#,
            Some("one"),
        )
        .expect_err("duplicate name must fail");

        assert!(error.contains("duplicate AXE Store name"));
    }

    #[test]
    fn selects_by_alias() {
        let packages = parse_package_set(
            br#"{
                "python": {
                    "id": "runtime/python",
                    "name": "python",
                    "aliases": ["python3"],
                    "synopsis": null,
                    "channels": {"stable": "3.14.7"},
                    "targets": ["x86_64-linux"],
                    "artifact": {"type": "package", "entrypoint": "bin/python3.14"}
                }
            }"#,
            Some("python3"),
        )
        .expect("select by alias must succeed");

        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].1.name, "python");
    }
}
