use crate::ui::iced::Subscription;
use crate::ui::{Task, widget};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc;

use crate::config::IconSizes;
use crate::tab;

#[cfg(feature = "gvfs")]
mod gvfs;

#[derive(Clone)]
pub struct MounterAuth {
    pub message: String,
    pub username_opt: Option<String>,
    pub domain_opt: Option<String>,
    pub password_opt: Option<String>,
    pub remember_opt: Option<bool>,
    pub anonymous_opt: Option<bool>,
}

// Custom debug for MounterAuth to hide password
impl fmt::Debug for MounterAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MounterAuth")
            .field("username_opt", &self.username_opt)
            .field("domain_opt", &self.domain_opt)
            .field(
                "password_opt",
                if self.password_opt.is_some() {
                    &"Some(*)"
                } else {
                    &"None"
                },
            )
            .field("remember_opt", &self.remember_opt)
            .field("anonymous_opt", &self.anonymous_opt)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub enum MounterItem {
    #[cfg(feature = "gvfs")]
    Gvfs(gvfs::Item),
    #[allow(dead_code)]
    None,
}

impl MounterItem {
    pub fn name(&self) -> String {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.name(),
            Self::None => unreachable!(),
        }
    }

    pub fn uri(&self) -> String {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.uri(),
            Self::None => unreachable!(),
        }
    }

    /// A stable identity: the volume's id for a volume, the mount root's URI
    /// for a mount. Unlike [`Self::uri`], never empty for a volume that has
    /// no activation root, as most local drives have none.
    pub fn id(&self) -> String {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.id(),
            Self::None => unreachable!(),
        }
    }

    pub fn is_mounted(&self) -> bool {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.is_mounted(),
            Self::None => unreachable!(),
        }
    }

    pub fn icon(&self, symbolic: bool) -> Option<widget::icon::Handle> {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.icon(symbolic),
            Self::None => unreachable!(),
        }
    }

    /// Path of this item's icon; see `gvfs::Item::icon_path`.
    pub fn icon_path(&self, symbolic: bool) -> Option<PathBuf> {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.icon_path(symbolic),
            Self::None => unreachable!(),
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.path(),
            Self::None => unreachable!(),
        }
    }

    pub fn is_remote(&self) -> bool {
        match self {
            #[cfg(feature = "gvfs")]
            Self::Gvfs(item) => item.is_remote(),
            Self::None => unreachable!(),
        }
    }
}

pub type MounterItems = Vec<MounterItem>;

#[derive(Clone, Debug)]
pub enum MounterMessage {
    Items(MounterItems),
    MountResult(MounterItem, Result<bool, String>),
    NetworkAuth(String, MounterAuth, mpsc::Sender<MounterAuth>),
    NetworkResult(String, Result<bool, String>),
}

/// How a mount, network-drive connect or unmount ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The user called it off, e.g. dismissed the password prompt.
    Cancelled,
    Failed,
}

pub trait Mounter: Send + Sync {
    /// Mounts `item`; resolves once that is done, with how it ended.
    fn mount(&self, item: MounterItem) -> Task<Outcome>;
    /// Connects the network drive at `uri`; resolves once that is done,
    /// with how it ended.
    fn network_drive(&self, uri: String) -> Task<Outcome>;
    fn network_scan(&self, uri: &str, sizes: IconSizes) -> Option<Result<Vec<tab::Item>, String>>;
    fn dir_info(&self, uri: &str) -> Option<(String, String, Option<PathBuf>)>;
    /// Unmounts or ejects `item`; resolves once that is done, with how it
    /// ended.
    fn unmount(&self, item: MounterItem) -> Task<Outcome>;
    fn subscription(&self) -> Subscription<MounterMessage>;
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MounterKey(pub &'static str);
pub type MounterMap = BTreeMap<MounterKey, Box<dyn Mounter>>;
pub type Mounters = Arc<MounterMap>;

pub fn mounters() -> Mounters {
    #[allow(unused_mut)]
    let mut mounters = MounterMap::new();

    #[cfg(feature = "gvfs")]
    {
        mounters.insert(MounterKey("gvfs"), Box::new(gvfs::Gvfs::new()));
    }

    Mounters::new(mounters)
}

pub static MOUNTERS: LazyLock<Mounters> = LazyLock::new(mounters);
