//! Drives, volumes and network locations, as a service: asked by plain
//! requests, answering with plain data. What the app makes of it, icons and
//! messages, is the app's: see [`ui`].

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc;

use crate::mime_app::Mime;

#[cfg(feature = "gvfs")]
mod gvfs;
pub mod ui;

/// What a mounter's requests answer with, later.
pub type Answer<T> = Pin<Box<dyn Future<Output = T> + Send>>;

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

/// One entry of a network location.
#[derive(Clone, Debug)]
pub struct NetworkEntry {
    pub name: String,
    pub display_name: String,
    pub uri: String,
    /// Where it is on this machine, for a location that maps to a path.
    pub path: Option<PathBuf>,
    pub is_dir: bool,
    pub size: Option<u64>,
    /// Modified, in seconds since the Unix epoch.
    pub mtime: u64,
    /// Its type as the server reports it; `None` for a folder.
    pub mime: Option<Mime>,
    pub hidden: bool,
    pub icon: IconRef,
}

/// An icon as a mounter names it: theme names to try in turn, or an image
/// file.
#[derive(Clone, Debug, Default)]
pub struct IconRef {
    pub names: Vec<String>,
    pub file: Option<PathBuf>,
}

/// A network location's entries.
#[derive(Clone, Debug, Default)]
pub struct NetworkListing {
    pub entries: Vec<NetworkEntry>,
    /// Every entry is a place to go into, as the network's own root lists.
    pub all_dirs: bool,
    /// The location is on a remote filesystem: its folders are not walked
    /// for their size.
    pub remote: bool,
}

/// What a network location is: its URI once followed, its name, and where
/// it is on this machine when it maps to a path.
pub type DirInfo = (String, String, Option<PathBuf>);

pub trait Mounter: Send + Sync {
    /// Mounts `item`; resolves once that is done, with how it ended.
    fn mount(&self, item: MounterItem) -> Answer<Outcome>;
    /// Connects the network drive at `uri`; resolves once that is done,
    /// with how it ended.
    fn network_drive(&self, uri: String) -> Answer<Outcome>;
    /// Lists `uri`: `None` when this mounter does not answer for it.
    fn network_scan(&self, uri: &str) -> Answer<Option<Result<NetworkListing, String>>>;
    fn dir_info(&self, uri: &str) -> Answer<Option<DirInfo>>;
    /// Unmounts or ejects `item`; resolves once that is done, with how it
    /// ended.
    fn unmount(&self, item: MounterItem) -> Answer<Outcome>;
    /// Asks for the mounts and volumes again: they come as an event.
    fn rescan(&self);
    /// The next thing that happened: the items changed, a mount ended, a
    /// password is wanted. `None` once there will be no more.
    fn next_event(&self) -> Answer<Option<MounterMessage>>;
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

/// What the network location `uri` is, from the first mounter that knows.
pub async fn dir_info(uri: &str) -> Option<DirInfo> {
    for mounter in MOUNTERS.values() {
        if let Some(info) = mounter.dir_info(uri).await {
            return Some(info);
        }
    }
    None
}
