use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

pub fn directory() -> Result<PathBuf> {
    let base = ["GITA_PROJECT_HOME", "XDG_CONFIG_HOME"]
        .iter()
        .find_map(|key| {
            env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            if cfg!(windows) {
                dirs::config_dir()
            } else {
                dirs::home_dir().map(|p| p.join(".config"))
            }
        })
        .context("Cannot find the configuration directory")?;
    Ok(base.join("gita"))
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct Repo {
    pub path: PathBuf,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct Group {
    pub path: Option<PathBuf>,
    pub repos: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    #[serde(default)]
    pub repos: IndexMap<String, Repo>,
    #[serde(default)]
    pub groups: IndexMap<String, Group>,
    #[serde(default)]
    pub context: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            repos: IndexMap::new(),
            groups: IndexMap::new(),
            context: None,
        }
    }
}

impl State {
    pub fn read(dir: &Path) -> Result<Self> {
        let file = dir.join("repos.json");
        let Some(data) = read_bytes(&file)? else {
            return Ok(Self::default());
        };
        let version: Value = decode(&file, &data)?;
        if version["version"].as_u64().is_some_and(|v| v > 1) {
            bail!("{} was created with a newer gita", file.display());
        }
        decode(&file, &data)
    }

    pub fn save(&mut self, dir: &Path) -> Result<()> {
        self.groups
            .retain(|_, group| group.repos.iter().any(|name| self.repos.contains_key(name)));
        if self
            .context
            .as_ref()
            .is_some_and(|name| name != "auto" && !self.groups.contains_key(name))
        {
            self.context = None;
        }
        save(&dir.join("repos.json"), self)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CmdSpec {
    Line(String),
    Argv(Vec<String>),
}

#[derive(Clone, Deserialize, Serialize)]
pub struct CmdDef {
    pub cmd: CmdSpec,
    pub help: Option<String>,
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub disable_async: bool,
    #[serde(default)]
    pub shell: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Auto,
    Gix,
    Git,
}

pub const INFO: [&str; 5] = ["branch", "branch_name", "commit_msg", "commit_time", "path"];

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub jobs: Option<usize>,
    pub backend: Backend,
    pub colors: IndexMap<String, String>,
    pub symbols: IndexMap<String, String>,
    pub layout: IndexMap<String, usize>,
    pub info_items: Option<Vec<String>>,
    pub commands: IndexMap<String, CmdDef>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

impl Settings {
    pub fn read(dir: &Path) -> Result<Self> {
        let settings: Self = read(&dir.join("settings.json"))?;
        if settings.jobs == Some(0) {
            bail!("settings.json: jobs must be positive");
        }
        if let Some(items) = &settings.info_items {
            for item in items {
                if !INFO.contains(&item.as_str()) {
                    bail!("settings.json: unknown info item {item}");
                }
            }
        }
        Ok(settings)
    }

    pub fn items(&self) -> Vec<String> {
        self.info_items.clone().unwrap_or_else(|| {
            ["branch", "commit_msg", "commit_time"]
                .map(str::to_owned)
                .into()
        })
    }

    pub fn jobs(&self, cli: Option<usize>) -> Result<Option<usize>> {
        if cli.is_some() {
            return Ok(cli);
        }
        match env::var("GITA_JOBS").ok().filter(|s| !s.is_empty()) {
            Some(value) => Ok(Some(
                value
                    .parse::<std::num::NonZeroUsize>()
                    .context("GITA_JOBS must be positive")?
                    .get(),
            )),
            None => Ok(self.jobs),
        }
    }

    pub fn backend(&self) -> Result<Backend> {
        match env::var("GITA_BACKEND").ok().as_deref() {
            None | Some("") => Ok(self.backend),
            Some("auto") => Ok(Backend::Auto),
            Some("gix") => Ok(Backend::Gix),
            Some("git") => Ok(Backend::Git),
            Some(value) => bail!("Invalid GITA_BACKEND: {value}"),
        }
    }
}

pub fn lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    let f = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".lock"))?;
    fs4::fs_std::FileExt::lock_exclusive(&f)?;
    Ok(f)
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("Cannot read {}", path.display())),
    }
}

fn decode<T: DeserializeOwned>(path: &Path, data: &[u8]) -> Result<T> {
    serde_json::from_slice(data).with_context(|| format!("Invalid JSON in {}", path.display()))
}

pub fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    read_bytes(path)?.map_or_else(|| Ok(T::default()), |data| decode(path, &data))
}

pub fn save(path: &Path, data: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Missing configuration directory")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, data)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .with_context(|| format!("Cannot replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip_preserves_order_and_stale_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::read(dir.path()).unwrap();
        assert!(state.repos.is_empty() && state.groups.is_empty());
        state.repos.insert(
            "z".into(),
            Repo {
                path: dir.path().join("z"),
                ..Repo::default()
            },
        );
        state.repos.insert(
            "a".into(),
            Repo {
                path: dir.path().join("a"),
                ..Repo::default()
            },
        );
        state.groups.insert(
            "active".into(),
            Group {
                path: None,
                repos: vec!["z".into(), "stale".into()],
            },
        );
        state.groups.insert(
            "prune".into(),
            Group {
                path: None,
                repos: vec!["stale".into()],
            },
        );
        state.context = Some("prune".into());
        state.save(dir.path()).unwrap();
        let data = fs::read_to_string(dir.path().join("repos.json")).unwrap();
        assert!(data.starts_with("{\n  \"version\": 1,"));
        assert!(data.ends_with("}\n"));
        let state = State::read(dir.path()).unwrap();
        assert_eq!(
            state.repos.keys().map(String::as_str).collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert_eq!(state.groups["active"].repos, ["z", "stale"]);
        assert!(!state.groups.contains_key("prune") && state.context.is_none());
    }

    #[test]
    fn absent_settings_have_defaults_and_partial_values_are_retained() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = Settings::read(dir.path()).unwrap();
        assert_eq!(defaults.items(), ["branch", "commit_msg", "commit_time"]);
        fs::write(
            dir.path().join("settings.json"),
            r#"{"layout":{"path":12},"future":true}"#,
        )
        .unwrap();
        let custom = Settings::read(dir.path()).unwrap();
        assert_eq!(custom.items(), defaults.items());
        assert_eq!(custom.layout["path"], 12);
        assert_eq!(custom._extra["future"], true);
    }
}
