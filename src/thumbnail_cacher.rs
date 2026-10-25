//! Thumbnails cached as the freedesktop.org Thumbnail Managing Standard
//! says, through xdg-kit: the personal cache other programs share, and the
//! markers that say a thumbnail could not be made.
//! <https://specifications.freedesktop.org/thumbnail-spec/latest/>

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use image::DynamicImage;
use tempfile::NamedTempFile;
use xdg_kit::basedir::Durability;
use xdg_kit::thumbnails::{Application, SourceInfo, ThumbnailCache, ThumbnailSource};

pub use xdg_kit::thumbnails::ThumbnailSize;

/// The personal cache; none without base directories.
static CACHE: LazyLock<Option<ThumbnailCache>> = LazyLock::new(|| {
    let cache = crate::xdg_dirs::BASE_DIRS.as_ref().map(ThumbnailCache::new);
    if cache.is_none() {
        log::warn!("no thumbnail cache directory, thumbnails will not be cached");
    }
    cache
});

/// The name failure markers are filed under. It carries the version, so a
/// new version tries again what an older one could not make.
static APPLICATION: LazyLock<Option<Application>> =
    LazyLock::new(|| Application::new(format!("earth-files-{}", env!("CARGO_PKG_VERSION"))).ok());

/// One file's thumbnail at one size, in the cache.
pub struct ThumbnailCacher {
    cache: &'static ThumbnailCache,
    /// The file as asked for, shown as is when it is a thumbnail itself.
    file_path: PathBuf,
    source: ThumbnailSource,
    /// The file's metadata when this was made. A new thumbnail is stamped
    /// with it, so it never claims to be newer than what it was made from.
    info: SourceInfo,
    size: ThumbnailSize,
}

impl ThumbnailCacher {
    pub fn new(file_path: &Path, size: ThumbnailSize) -> Result<Self, String> {
        let cache = CACHE
            .as_ref()
            .ok_or_else(|| "no thumbnail cache directory".to_owned())?;
        Self::in_cache(cache, file_path, size)
    }

    /// [`Self::new`] in `cache`.
    fn in_cache(
        cache: &'static ThumbnailCache,
        file_path: &Path,
        size: ThumbnailSize,
    ) -> Result<Self, String> {
        // Keyed by the real path, symlinks resolved, as this app has always
        // keyed it, so thumbnails made before are still found
        let real = fs::canonicalize(file_path)
            .map_err(|err| format!("failed to resolve {}: {err}", file_path.display()))?;
        let source = ThumbnailSource::local(&real)
            .map_err(|err| format!("failed to name {}: {err}", real.display()))?;
        let info = SourceInfo::of_local(&real)
            .map_err(|err| format!("failed to read {}: {err}", real.display()))?;
        Ok(Self {
            cache,
            file_path: file_path.to_path_buf(),
            source,
            info,
            size,
        })
    }

    pub fn get_cached_thumbnail(&self) -> CachedThumbnail {
        // A thumbnail is shown as itself: the standard forbids thumbnails of
        // thumbnails
        if self
            .source
            .path()
            .is_some_and(|path| self.cache.is_thumbnail_path(path))
        {
            return CachedThumbnail::Valid((self.file_path.clone(), None));
        }

        match self.cache.find(&self.source, self.size, &self.info) {
            Ok(Some(thumbnail)) => {
                return CachedThumbnail::Valid((
                    thumbnail.path().to_path_buf(),
                    Some(thumbnail.size()),
                ));
            }
            Ok(None) => {}
            Err(err) => log::warn!(
                "failed to look up the thumbnail of {}: {err}",
                self.file_path.display()
            ),
        }

        // Made before and failed, and the file has not changed since
        if let Some(application) = APPLICATION.as_ref()
            && let Ok(Some(_)) = self.cache.failed(&self.source, application, &self.info)
        {
            return CachedThumbnail::Failed;
        }

        CachedThumbnail::RequiresUpdate(self.size)
    }

    /// Installs `temp_file`, a PNG a thumbnailer wrote, as the thumbnail.
    pub fn update_with_temp_file(
        &self,
        temp_file: NamedTempFile,
    ) -> Result<PathBuf, Box<dyn Error>> {
        self.install(temp_file.path())
    }

    /// Installs `image`, scaled down to the size, as the thumbnail.
    pub fn update_with_image(&self, image: DynamicImage) -> Result<PathBuf, Box<dyn Error>> {
        let edge = self.size.max_dimension();
        let temp_file = tempfile::Builder::new()
            .prefix("earth-files-")
            .suffix(".png")
            .tempfile()?;
        image
            .thumbnail(edge, edge)
            .into_rgba8()
            .save_with_format(temp_file.path(), image::ImageFormat::Png)?;
        self.install(temp_file.path())
    }

    /// Records that no thumbnail could be made, so it is not tried again
    /// until the file changes.
    pub fn create_fail_marker(&self) -> Result<(), Box<dyn Error>> {
        let application = APPLICATION
            .as_ref()
            .ok_or("no application name for the failure marker")?;
        let (_, durability) = self
            .cache
            .mark_failed(&self.source, &self.info, application)?;
        not_synced(durability, &self.file_path);
        Ok(())
    }

    fn install(&self, finished: &Path) -> Result<PathBuf, Box<dyn Error>> {
        let (path, durability) =
            self.cache
                .install(finished, &self.source, &self.info, self.size)?;
        not_synced(durability, &self.file_path);
        Ok(path)
    }
}

/// Logs a thumbnail that is in place but whose folder could not be synced:
/// it is there, and only a crash could lose it, which costs a regeneration.
fn not_synced(durability: Durability, file_path: &Path) {
    if let Durability::NotSynced(err) = durability {
        log::debug!(
            "thumbnail of {} is in place but not synced: {err}",
            file_path.display()
        );
    }
}

/// The size class for thumbnails up to `pixel_size` across.
pub fn size_for(pixel_size: u32) -> ThumbnailSize {
    ThumbnailSize::ALL
        .into_iter()
        .find(|size| pixel_size <= size.max_dimension())
        .unwrap_or(ThumbnailSize::XXLarge)
}

pub enum CachedThumbnail {
    /// The cached thumbnail is valid and should be used with size if known.
    Valid((PathBuf, Option<ThumbnailSize>)),
    /// The cached thumbnail doesn't exist or it's invalid and
    /// needs to be recreated with the pixel size.
    RequiresUpdate(ThumbnailSize),
    // The cached thumbnail is in a failed state.
    // This means it failed to create by earth-files in the past
    // and shouldn't be tried again.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache file name is the lowercase hex MD5 of the file's URI, the
    /// same name other file managers compute: a different one would quietly
    /// stop the cache being shared. Reference value from
    /// `printf 'file:///home/user/test.png' | md5sum`.
    #[test]
    fn the_cache_name_is_shared_with_other_file_managers() {
        let source = ThumbnailSource::local("/home/user/test.png").unwrap();
        assert_eq!(source.cache_name(), "03223f4f10458a8b5d14327f3ae23136.png");
    }

    /// Square brackets are percent-encoded, as GNOME's thumbnailer encodes
    /// them, so its thumbnails of such names are found too.
    #[test]
    fn square_brackets_are_encoded_as_gnome_encodes_them() {
        let source = ThumbnailSource::local("/home/user/a [1].png").unwrap();
        assert_eq!(source.uri(), "file:///home/user/a%20%5B1%5D.png");
    }

    /// A cache in a home of its own, kept for the test's run.
    fn cache_in(home: &Path) -> &'static ThumbnailCache {
        let base_dirs = xdg_kit::basedir::BaseDirs::from_vars(|name| {
            let path = match name {
                "HOME" => home.to_owned(),
                "XDG_CACHE_HOME" => home.join("cache"),
                _ => return None,
            };
            Some(path.into())
        })
        .unwrap();
        Box::leak(Box::new(ThumbnailCache::new(&base_dirs)))
    }

    #[test]
    fn a_made_thumbnail_is_found_until_its_file_changes() {
        let home = tempfile::tempdir().unwrap();
        let cache = cache_in(home.path());
        let photo = home.path().join("photo.png");
        DynamicImage::new_rgb8(300, 200).save(&photo).unwrap();

        let cacher = ThumbnailCacher::in_cache(cache, &photo, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            cacher.get_cached_thumbnail(),
            CachedThumbnail::RequiresUpdate(_)
        ));
        let made = cacher
            .update_with_image(image::open(&photo).unwrap())
            .unwrap();
        assert!(made.starts_with(home.path().join("cache/thumbnails/normal")));
        let (width, height) = image::image_dimensions(&made).unwrap();
        assert_eq!((width, height), (128, 85), "scaled down to the size");

        let again = ThumbnailCacher::in_cache(cache, &photo, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            again.get_cached_thumbnail(),
            CachedThumbnail::Valid((path, Some(ThumbnailSize::Normal))) if path == made
        ));

        // A later change to the file makes the thumbnail stale
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&photo)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let changed = ThumbnailCacher::in_cache(cache, &photo, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            changed.get_cached_thumbnail(),
            CachedThumbnail::RequiresUpdate(_)
        ));
    }

    #[test]
    fn a_failure_is_remembered_until_its_file_changes() {
        let home = tempfile::tempdir().unwrap();
        let cache = cache_in(home.path());
        let broken = home.path().join("broken.png");
        std::fs::write(&broken, "not a picture").unwrap();

        let cacher = ThumbnailCacher::in_cache(cache, &broken, ThumbnailSize::Normal).unwrap();
        cacher.create_fail_marker().unwrap();
        let again = ThumbnailCacher::in_cache(cache, &broken, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            again.get_cached_thumbnail(),
            CachedThumbnail::Failed
        ));

        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&broken)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let changed = ThumbnailCacher::in_cache(cache, &broken, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            changed.get_cached_thumbnail(),
            CachedThumbnail::RequiresUpdate(_)
        ));
    }

    #[test]
    fn a_thumbnail_is_shown_as_itself() {
        let home = tempfile::tempdir().unwrap();
        let cache = cache_in(home.path());
        let photo = home.path().join("photo.png");
        DynamicImage::new_rgb8(300, 200).save(&photo).unwrap();
        let made = ThumbnailCacher::in_cache(cache, &photo, ThumbnailSize::Normal)
            .unwrap()
            .update_with_image(image::open(&photo).unwrap())
            .unwrap();

        let of_thumbnail = ThumbnailCacher::in_cache(cache, &made, ThumbnailSize::Normal).unwrap();
        assert!(matches!(
            of_thumbnail.get_cached_thumbnail(),
            CachedThumbnail::Valid((path, None)) if path == made
        ));
    }

    #[test]
    fn a_pixel_size_picks_the_smallest_class_that_holds_it() {
        assert_eq!(size_for(64), ThumbnailSize::Normal);
        assert_eq!(size_for(128), ThumbnailSize::Normal);
        assert_eq!(size_for(129), ThumbnailSize::Large);
        assert_eq!(size_for(512), ThumbnailSize::XLarge);
        assert_eq!(size_for(4000), ThumbnailSize::XXLarge);
    }
}
