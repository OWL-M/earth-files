use crate::ui::widget;
use image::ImageReader;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes per pixel in RGBA format (Red, Green, Blue, Alpha = 4 bytes)
pub const RGBA_BYTES_PER_PIXEL: u64 = 4;

/// System memory reserve in MB to maintain for system stability (prevents thrashing)
/// Note: RAM checking is currently only available on Linux, via /proc/meminfo.
/// On Windows and macOS, only GPU buffer limits are enforced.
const SYSTEM_MEMORY_RESERVE_MB: u64 = 500;

/// Maximum memory allocation for gallery image decoding in MB.
/// Gallery mode uses the full memory budget since only one image decodes at a time.
/// This matches the ThumbCfg max_mem_mb budget for consistency.
const GALLERY_MEMORY_LIMIT_MB: u64 = 2000;

/// Threshold for considering an image "large" requiring GPU tiling
/// Atlas fragment/tile size in pixels. Large images are split into fragments of this size.
/// Must match the atlas size constant in `iced_wgpu`'s `src/image/atlas.rs`.
pub const ATLAS_FRAGMENT_SIZE: u32 = 4096;

/// Conversion factor: 1 MB = 1024 * 1024 bytes (binary megabyte, used for RAM calculations)
pub const MB_TO_BYTES: u64 = 1024 * 1024;

/// Conversion factor: 1 MB = 1000 * 1000 bytes (decimal megabyte, used by image crate)
/// The image crate's memory limits use decimal MB, not binary MB.
pub const DECIMAL_MB_TO_BYTES: u64 = 1000 * 1000;

/// Scale factor for HiDPI displays - decode at higher resolution than display size
/// for better quality on high-DPI screens. 1.5x provides good balance between
/// quality and memory usage and also prevets re-decoding on small windows size adjustments.
const DISPLAY_SCALE_FACTOR: f32 = 1.5;

/// Calculate optimal target dimensions for decoding based on display size.
/// Returns None if no resizing is needed (image is smaller than display).
///
/// This helps reduce memory usage by decoding large images at a resolution
/// appropriate for the display, rather than always using full resolution.
pub fn calculate_target_dimensions(
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Option<(u32, u32)> {
    let target_width = (display_width as f32 * DISPLAY_SCALE_FACTOR) as u32;
    let target_height = (display_height as f32 * DISPLAY_SCALE_FACTOR) as u32;

    if image_width <= target_width && image_height <= target_height {
        return None;
    }

    let image_aspect = image_width as f32 / image_height as f32;
    let target_aspect = target_width as f32 / target_height as f32;

    let (new_width, new_height) = if image_aspect > target_aspect {
        let w = target_width;
        let h = (target_width as f32 / image_aspect) as u32;
        (w, h)
    } else {
        let h = target_height;
        let w = (target_height as f32 * image_aspect) as u32;
        (w, h)
    };

    let new_width = new_width.max(1);
    let new_height = new_height.max(1);

    log::info!(
        "Calculated target dimensions: {}x{} -> {}x{} (display: {}x{}, scale: {}x)",
        image_width,
        image_height,
        new_width,
        new_height,
        display_width,
        display_height,
        DISPLAY_SCALE_FACTOR
    );

    Some((new_width, new_height))
}

/// Check if an image's dimensions would exceed the available memory budget.
/// Returns true if the image is too large to decode.
pub fn exceeds_memory_limit(width: u32, height: u32, memory_limit_mb: u64) -> bool {
    let Some(bytes_needed) = calculate_image_memory(width, height) else {
        // Overflow in calculation means it definitely exceeds any reasonable limit
        return true;
    };

    let max_bytes = memory_limit_mb * MB_TO_BYTES;
    bytes_needed > max_bytes
}

/// Check if an image should use GPU tiling for display.
/// Images larger than the atlas fragment size need to be split into tiles for GPU upload.
pub fn should_use_tiling(width: u32, height: u32) -> bool {
    width > ATLAS_FRAGMENT_SIZE || height > ATLAS_FRAGMENT_SIZE
}

/// Determine if an image should use the dedicated worker for thumbnail generation.
/// Returns (use_dedicated_worker, effective_max_mb, effective_jobs).
///
/// Large images that exceed per-worker memory budget get routed to a dedicated worker
/// with full memory budget. Smaller images use the normal parallel worker pool.
pub fn should_use_dedicated_worker(
    width: u32,
    height: u32,
    total_budget_mb: u64,
    parallel_workers: usize,
) -> (bool, u64, usize) {
    if width == 0 || height == 0 {
        log::warn!(
            "Invalid image dimensions {}x{}, using normal queue",
            width,
            height
        );
        return (false, total_budget_mb, parallel_workers);
    }

    let Some(bytes_needed) = calculate_image_memory(width, height) else {
        log::warn!(
            "Image dimensions {}x{} overflow memory calculation, using normal queue",
            width,
            height
        );
        return (false, total_budget_mb, parallel_workers);
    };

    let mb_needed = bytes_needed / MB_TO_BYTES;
    let per_worker_budget_mb = total_budget_mb / parallel_workers as u64;

    if mb_needed > per_worker_budget_mb {
        log::info!(
            "Large image {}x{} needs {}MB (exceeds per-worker {}MB), using dedicated worker",
            width,
            height,
            mb_needed,
            per_worker_budget_mb
        );
        // Use dedicated worker with full budget
        (true, total_budget_mb, 1)
    } else {
        log::debug!(
            "Normal image {}x{} needs {}MB (within per-worker {}MB), using parallel workers",
            width,
            height,
            mb_needed,
            per_worker_budget_mb
        );
        // Use parallel worker pool with shared budget
        (false, total_budget_mb, parallel_workers)
    }
}

/// Get the dimensions of an image without fully decoding it
pub fn get_image_dimensions(path: &Path) -> Option<(u32, u32)> {
    match ImageReader::open(path) {
        Ok(reader) => match reader.into_dimensions() {
            Ok((width, height)) => {
                log::debug!(
                    "Image dimensions: {}x{} for {}",
                    width,
                    height,
                    path.display()
                );
                Some((width, height))
            }
            Err(e) => {
                log::warn!("Failed to get dimensions for {}: {}", path.display(), e);
                None
            }
        },
        Err(e) => {
            log::warn!("Failed to open image reader for {}: {}", path.display(), e);
            None
        }
    }
}

/// Calculate the memory required to decode an image in bytes.
/// Returns None if the calculation overflows.
fn calculate_image_memory(width: u32, height: u32) -> Option<u64> {
    let pixels = (width as u64).checked_mul(height as u64)?;
    pixels.checked_mul(RGBA_BYTES_PER_PIXEL)
}

/// Check if there's `bytes_needed` of system RAM to spare.
/// Returns: (has_memory, error_message)
fn check_ram_bytes(bytes_needed: u64) -> (bool, Option<String>) {
    let mb_needed = bytes_needed / MB_TO_BYTES;

    match fs::read_to_string("/proc/meminfo") {
        Ok(meminfo) => {
            // MemAvailable includes reclaimable cache and is the best estimate of
            // actually available memory for new allocations
            let available_kb = mem_available_kb(&meminfo).unwrap_or(0);
            let available_bytes = available_kb * 1024;

            // Maintain system reserve to prevent thrashing and OOM killer
            let min_reserve_bytes = SYSTEM_MEMORY_RESERVE_MB * MB_TO_BYTES;
            let usable_bytes = available_bytes.saturating_sub(min_reserve_bytes);

            if bytes_needed > usable_bytes {
                let available_mb = available_bytes / MB_TO_BYTES;
                let error_msg = format!(
                    "Insufficient memory: need {}MB, available {}MB. Try closing other applications.",
                    mb_needed, available_mb
                );
                log::warn!("{}", error_msg);
                return (false, Some(error_msg));
            }

            (true, None)
        }
        Err(e) => {
            log::warn!("Failed to read /proc/meminfo: {}. Skipping RAM check.", e);
            // Graceful fallback: assume RAM is available
            (true, None)
        }
    }
}

/// The `MemAvailable` line of `/proc/meminfo`'s text, in kB.
fn mem_available_kb(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    line.trim().strip_suffix("kB")?.trim().parse().ok()
}

/// The most bytes a decoded image takes per byte of its 8-bit RGBA form:
/// four `f32` channels to four `u8` ones.
const DECODED_BYTES_PER_RGBA8_BYTE: u64 = 4;

/// Bytes per pixel of the floating-point image `image::imageops::resize`
/// works through: four `f32` channels.
const RESIZE_SCRATCH_BYTES_PER_PIXEL: u64 = 16;

/// The most memory decoding a `width` by `height` image to `target`
/// dimensions holds at once. Two moments compete:
///
/// - Decoding: the image as decoded, up to four times the 8-bit RGBA size
///   for a 32-bit float RGBA image such as an OpenEXR one, beside its 8-bit
///   RGBA copy. Which the file holds is not known before decoding, so the
///   largest is assumed.
/// - Resizing: the 8-bit image, the resize's floating-point intermediate,
///   which is the full width by the target height, and the result.
///
/// `None` if it overflows.
pub fn decode_peak_bytes(width: u32, height: u32, target: Option<(u32, u32)>) -> Option<u64> {
    let full = calculate_image_memory(width, height)?;
    let decoding = full.checked_mul(DECODED_BYTES_PER_RGBA8_BYTE + 1)?;
    let Some((target_w, target_h)) = target else {
        return Some(decoding);
    };
    let scratch = u64::from(width)
        .checked_mul(u64::from(target_h))?
        .checked_mul(RESIZE_SCRATCH_BYTES_PER_PIXEL)?;
    let resizing = full
        .checked_add(scratch)?
        .checked_add(calculate_image_memory(target_w, target_h)?)?;
    Some(decoding.max(resizing))
}

/// Decode a large image asynchronously in a blocking thread pool.
///
/// This function is used for gallery mode where full-resolution images need to be loaded.
/// It uses the full memory budget (GALLERY_MEMORY_LIMIT_MB) since only one image
/// decodes at a time in gallery mode.
///
/// `order` is the gallery's own count of the decodes it asked for, from
/// [`LargeImageManager::decode_order`]: a decode it asked for before another
/// is skipped, as that gallery has moved on. Another tab's gallery asking
/// does not count, as this one still shows its image.
pub async fn decode_large_image(
    path: PathBuf,
    target_dimensions: Option<(u32, u32)>,
    order: Arc<AtomicU64>,
) -> Decoded {
    // Numbered before waiting, so that a decode asked for later supersedes it
    let ticket = order.fetch_add(1, Ordering::Relaxed) + 1;
    // One at a time, in all galleries: flipping through one starts a decode
    // per image, and each holds a full-size image while it runs, which
    // nothing can stop
    let Ok(permit) = DECODE_TURN.acquire().await else {
        return Decoded::Superseded;
    };
    if order.load(Ordering::Relaxed) != ticket {
        // Asked for an image the gallery has since moved on from
        return Decoded::Superseded;
    }
    let decoded = decode_now(path, target_dimensions).await;
    drop(permit);
    match decoded {
        Some(Ok(done)) => Decoded::Done(done),
        Some(Err(Changed)) => Decoded::Changed,
        None => Decoded::Failed,
    }
}

/// What came of [`decode_large_image`].
pub enum Decoded {
    /// The path, its decoded size, its pixels, and which file they are of.
    Done((PathBuf, u32, u32, Vec<u8>, FileStamp)),
    Failed,
    /// Not decoded, as the gallery asked for another image before its turn.
    Superseded,
    /// The file was replaced while it was being decoded.
    Changed,
}

/// The file being decoded changed under the decode.
struct Changed;

/// Taken by the decode under way.
static DECODE_TURN: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

async fn decode_now(
    path: PathBuf,
    target_dimensions: Option<(u32, u32)>,
) -> Option<Result<(PathBuf, u32, u32, Vec<u8>, FileStamp), Changed>> {
    // Decode image in blocking thread pool (CPU-intensive work should not block)
    tokio::task::spawn_blocking(move || {
        log::info!("Starting async decode of {}", path.display());
        // Which file is read, looked at before and after: one replaced in
        // between gave pixels of neither for certain
        let stamp = file_stamp(&path);

        // Use ImageReader with explicit memory limits to avoid "Memory limit exceeded" errors
        // Gallery mode uses the full memory budget since only one image decodes at a time
        match image::ImageReader::open(&path) {
            Ok(reader) => {
                match reader.with_guessed_format() {
                    Ok(mut reader) => {
                        // Note: image crate uses decimal MB (1000^2), not binary MB (1024^2)
                        let mut limits = image::Limits::default();
                        limits.max_alloc = Some(GALLERY_MEMORY_LIMIT_MB * DECIMAL_MB_TO_BYTES);
                        reader.limits(limits);

                        match reader.decode() {
                            Ok(img) => {
                                let rgba = img.into_rgba8();
                                let orig_width = rgba.width();
                                let orig_height = rgba.height();

                                // Resize if target dimensions provided
                                let (final_img, width, height) = if let Some((target_w, target_h)) = target_dimensions {
                                    log::info!(
                                        "Resizing {}x{} -> {}x{} for memory optimization: {}",
                                        orig_width, orig_height, target_w, target_h,
                                        path.display()
                                    );

                                    // Use Lanczos3 for high-quality downsampling
                                    let resized = image::imageops::resize(
                                        &rgba,
                                        target_w,
                                        target_h,
                                        image::imageops::FilterType::Lanczos3,
                                    );

                                    let resized_w = resized.width();
                                    let resized_h = resized.height();

                                    log::info!(
                                        "Resize complete: {}x{} image now uses ~{} MB instead of ~{} MB",
                                        resized_w, resized_h,
                                        (resized_w as u64 * resized_h as u64 * 4) / MB_TO_BYTES,
                                        (orig_width as u64 * orig_height as u64 * 4) / MB_TO_BYTES
                                    );

                                    (resized, resized_w, resized_h)
                                } else {
                                    log::info!(
                                        "Decoded {}x{} image at full resolution: {}",
                                        orig_width, orig_height,
                                        path.display()
                                    );
                                    (rgba, orig_width, orig_height)
                                };

                                let pixels = final_img.into_raw();
                                if file_stamp(&path) != stamp {
                                    return Some(Err(Changed));
                                }
                                Some(Ok((path, width, height, pixels, stamp)))
                            }
                            Err(e) => {
                                log::warn!("Failed to decode {}: {}", path.display(), e);
                                None
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to guess format for {}: {}", path.display(), e);
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to open {}: {}", path.display(), e);
                None
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Manages state and operations for large image decoding in gallery mode
#[derive(Debug, Default)]
pub struct LargeImageManager {
    /// Paths of images currently being decoded
    decoding_images: HashSet<PathBuf>,
    /// Cache of decoded image handles
    decoded_images: HashMap<PathBuf, widget::image::Handle>,
    /// Display dimensions used for each decoded image (for resize detection)
    decoded_display_sizes: HashMap<PathBuf, (u32, u32)>,
    /// Errors encountered during decoding
    decode_errors: HashMap<PathBuf, String>,
    /// Generation counter for each decode to support cancellation.
    /// When a new decode is started for the same path, the generation is incremented.
    /// Only decodes matching the current generation are accepted when they complete.
    decode_generations: HashMap<PathBuf, u64>,
    /// Which file each path's latest decode was of. A file replaced at the
    /// same path is another image, and what was decoded of the old one is
    /// not shown for it.
    decode_stamps: HashMap<PathBuf, FileStamp>,
    /// Counts the decodes this gallery asked for, so that one it has moved
    /// on from is skipped; see [`decode_large_image`].
    decode_order: Arc<AtomicU64>,
}

/// What tells one file at a path from another put there in its place: its
/// inode, size and modification time. `None` when it cannot be read.
pub type FileStamp = Option<(u64, u64, Option<std::time::SystemTime>)>;

fn file_stamp(path: &Path) -> FileStamp {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.ino(), metadata.len(), metadata.modified().ok()))
}

impl LargeImageManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_decoding(&self, path: &Path) -> bool {
        self.decoding_images.contains(path)
    }

    pub fn get_decoded(&self, path: &Path) -> Option<&widget::image::Handle> {
        self.decoded_images.get(path)
    }

    pub fn get_error(&self, path: &Path) -> Option<&String> {
        self.decode_errors.get(path)
    }

    /// Whether `generation` is the latest decode asked for `path`.
    pub fn is_current(&self, path: &Path, generation: u64) -> bool {
        self.decode_generations.get(path) == Some(&generation)
    }

    /// This gallery's count of the decodes it asked for, for
    /// [`decode_large_image`].
    pub fn decode_order(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.decode_order)
    }

    /// Store a decoded image if the generation matches (not superseded by
    /// newer decode) and it is of the file the decode was asked for, as
    /// `stamp` says. Returns true if stored. One of a file since replaced is
    /// not, and the path is no longer decoding: asked for again, it is
    /// decoded afresh.
    pub fn store_decoded_with_generation(
        &mut self,
        path: PathBuf,
        handle: widget::image::Handle,
        display_size: Option<(u32, u32)>,
        generation: u64,
        stamp: FileStamp,
    ) -> bool {
        if self.decode_generations.get(&path) == Some(&generation)
            && self.decode_stamps.get(&path) != Some(&stamp)
        {
            log::info!("Discarding decode of {}, replaced since", path.display());
            self.decoding_images.remove(&path);
            self.decode_stamps.remove(&path);
            return false;
        }
        // Check if this decode is still current (not superseded by a newer one)
        if let Some(&current_gen) = self.decode_generations.get(&path)
            && generation != current_gen
        {
            log::info!(
                "Discarding outdated decode for {} (generation {} != current {})",
                path.display(),
                generation,
                current_gen
            );
            return false;
        }

        log::info!(
            "Storing decoded image for {} (generation {})",
            path.display(),
            generation
        );

        self.decoded_images.insert(path.clone(), handle);
        if let Some(size) = display_size {
            self.decoded_display_sizes.insert(path.clone(), size);
        }
        self.decoding_images.remove(&path);
        true
    }

    /// Forgets a decode that never ran, if it is still the current one for
    /// its path: not loading, and not failed either, so that the gallery
    /// coming back to it asks again.
    pub fn abandon_with_generation(&mut self, path: &Path, generation: u64) {
        if self.decode_generations.get(path) == Some(&generation) {
            self.decoding_images.remove(path);
        }
    }

    /// Ends a decode that failed, if it is still the current one for its
    /// path: the image is no longer loading, and says why. A superseded
    /// decode's failure leaves the newer one alone.
    pub fn fail_with_generation(&mut self, path: PathBuf, generation: u64, error: String) {
        if self.decode_generations.get(&path) == Some(&generation) {
            self.store_error(path, error);
        }
    }

    pub fn store_error(&mut self, path: PathBuf, error: String) {
        self.decode_errors.insert(path.clone(), error);
        self.decoding_images.remove(&path);
    }

    pub fn clear_error(&mut self, path: &Path) {
        self.decode_errors.remove(path);
    }

    pub fn clear_cache(&mut self) {
        log::info!(
            "Clearing {} cached images from large image manager",
            self.decoded_images.len()
        );
        self.decoded_images.clear();
    }

    pub fn cache_size(&self) -> usize {
        self.decoded_images.len()
    }

    pub fn cache_is_empty(&self) -> bool {
        self.decoded_images.is_empty()
    }

    /// Check if an image should be re-decoded due to display size increase.
    /// Returns true only if the display size has INCREASED by more than 20% in either dimension.
    /// Does NOT re-decode for smaller sizes (GPU can efficiently downscale).
    pub fn needs_redecode_for_size(
        &self,
        path: &Path,
        new_display_size: Option<(u32, u32)>,
    ) -> bool {
        let Some(new_size) = new_display_size else {
            return false;
        };

        let Some(&old_size) = self.decoded_display_sizes.get(path) else {
            return false;
        };

        const REDECODE_THRESHOLD: f32 = 0.2;

        let width_increase = (new_size.0 as f32 / old_size.0 as f32) - 1.0;
        let height_increase = (new_size.1 as f32 / old_size.1 as f32) - 1.0;

        let needs_redecode =
            width_increase > REDECODE_THRESHOLD || height_increase > REDECODE_THRESHOLD;

        if needs_redecode {
            log::info!(
                "Display size increased significantly for {}: {}x{} -> {}x{} (increase: {:.1}% width, {:.1}% height) - re-decoding at higher resolution",
                path.display(),
                old_size.0,
                old_size.1,
                new_size.0,
                new_size.1,
                width_increase * 100.0,
                height_increase * 100.0
            );
        } else if width_increase < -REDECODE_THRESHOLD || height_increase < -REDECODE_THRESHOLD {
            log::debug!(
                "Display size decreased for {}: {}x{} -> {}x{} (decrease: {:.1}% width, {:.1}% height) - keeping existing higher resolution",
                path.display(),
                old_size.0,
                old_size.1,
                new_size.0,
                new_size.1,
                width_increase * 100.0,
                height_increase * 100.0
            );
        }

        needs_redecode
    }

    /// Attempt to decode a large image, checking memory availability first.
    /// Returns (should_decode, target_dimensions, generation) tuple.
    pub fn try_decode(
        &mut self,
        path: &PathBuf,
        display_dimensions: Option<(u32, u32)>,
    ) -> (bool, Option<(u32, u32)>, u64) {
        self.clear_error(path);
        // Replaced since it was decoded, or while it was: neither the old
        // pixels nor a decode still under way are this file's
        let stamp = file_stamp(path);
        if self.decode_stamps.get(path) != Some(&stamp) {
            self.decoded_images.remove(path);
            self.decoded_display_sizes.remove(path);
            self.decoding_images.remove(path);
        }
        let is_currently_decoding = self.is_decoding(path);
        let needs_redecode = self.needs_redecode_for_size(path, display_dimensions);

        if is_currently_decoding && !needs_redecode {
            // Get current generation for the ongoing decode
            let generation = self.decode_generations.get(path).copied().unwrap_or(0);
            return (false, None, generation);
        }

        if self.get_decoded(path).is_some() && !needs_redecode && !is_currently_decoding {
            let generation = self.decode_generations.get(path).copied().unwrap_or(0);
            return (false, None, generation);
        }

        let Some((width, height)) = get_image_dimensions(path) else {
            self.store_error(path.clone(), "Failed to read image dimensions".to_string());
            return (false, None, 0);
        };

        let target_dimensions = if let Some((display_w, display_h)) = display_dimensions {
            calculate_target_dimensions(width, height, display_w, display_h)
        } else {
            None
        };

        // The decode holds the full-size image before resizing it, so it is
        // what it holds at its peak that has to fit, not the resized result
        let Some(peak) = decode_peak_bytes(width, height, target_dimensions) else {
            self.store_error(
                path.clone(),
                format!("Image dimensions too large: {width}x{height}"),
            );
            return (false, None, 0);
        };
        if !self.ensure_memory_available(path, peak) {
            return (false, None, 0);
        }

        // Increment generation counter (cancels any previous decode)
        let generation = self
            .decode_generations
            .entry(path.clone())
            .and_modify(|g| *g += 1)
            .or_insert(1);
        let generation = *generation;

        if is_currently_decoding {
            log::info!(
                "Cancelling previous decode for {} and starting new one (generation {})",
                path.display(),
                generation
            );
        }

        // Mark as decoding
        self.decoding_images.insert(path.clone());
        self.decode_stamps.insert(path.clone(), stamp);
        (true, target_dimensions, generation)
    }

    /// Check if sufficient memory is available, clearing cache if needed.
    /// Returns true if memory is available, false otherwise.
    fn ensure_memory_available(&mut self, path: &Path, bytes: u64) -> bool {
        let (has_memory, error_opt) = check_ram_bytes(bytes);

        if has_memory {
            return true;
        }

        if self.cache_is_empty() {
            if let Some(error_msg) = error_opt {
                self.store_error(path.to_path_buf(), error_msg);
                log::warn!(
                    "Cannot load {}: insufficient memory and cache is empty",
                    path.display()
                );
            }
            return false;
        }

        log::info!(
            "Insufficient memory, clearing {} cached images",
            self.cache_size()
        );
        self.clear_cache();

        let (has_memory_after_clear, error_opt_after) = check_ram_bytes(bytes);

        if has_memory_after_clear {
            log::info!("Memory available after cache clear, proceeding with decode");
            return true;
        }

        if let Some(error_msg) = error_opt_after {
            self.store_error(path.to_path_buf(), error_msg);
            log::warn!(
                "Cannot load {}: insufficient memory even after cache clear",
                path.display()
            );
        }
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_decode_peaks_at_its_larger_moment() {
        // Decoding: up to four times the 8-bit image as floats, and the
        // 8-bit image
        assert_eq!(super::decode_peak_bytes(10, 10, None), Some(2000));
        // Resizing: the image, 400; its intermediate, 10 wide by 1 high at
        // 16 bytes a pixel, 160; and the result, 20; less than decoding
        assert_eq!(super::decode_peak_bytes(10, 10, Some((5, 1))), Some(2000));
        // Here resizing is the larger: 400, 10 by 10 at 16, 1600, and 400
        assert_eq!(super::decode_peak_bytes(10, 10, Some((10, 10))), Some(2400));
        // Resizing: the image, its intermediate at 16 bytes a pixel of the
        // full width by the new height, and the result
        assert_eq!(
            super::decode_peak_bytes(6500, 3250, Some((6000, 3000))),
            Some(84_500_000 + 312_000_000 + 72_000_000)
        );
        assert_eq!(super::decode_peak_bytes(u32::MAX, u32::MAX, None), None);
    }

    /// What was decoded of a file is not shown for another put at its path,
    /// and a decode that failed can be asked for again
    #[test]
    fn a_file_replaced_at_its_path_is_decoded_afresh() {
        use image::DynamicImage;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        DynamicImage::new_rgb8(30, 20).save(&path).unwrap();
        let mut manager = super::LargeImageManager::new();

        let (decode, _, generation) = manager.try_decode(&path, None);
        assert!(decode);
        let handle = crate::ui::widget::image::Handle::from_rgba(1, 1, vec![0; 4]);
        let stamp = super::file_stamp(&path);
        assert!(manager.store_decoded_with_generation(
            path.clone(),
            handle.clone(),
            None,
            generation,
            stamp,
        ));
        assert!(!manager.try_decode(&path, None).0, "decoded already");

        // Another image at the same path
        std::fs::remove_file(&path).unwrap();
        DynamicImage::new_rgb8(40, 20).save(&path).unwrap();
        let (decode, _, again) = manager.try_decode(&path, None);
        assert!(decode, "the old pixels are not this file's");
        assert!(manager.get_decoded(&path).is_none());

        manager.fail_with_generation(path.clone(), again, "failed".to_string());
        assert!(!manager.is_decoding(&path), "no longer loading");
        assert!(manager.get_error(&path).is_some());
        assert!(manager.try_decode(&path, None).0, "asked again");
    }

    /// Pixels of a file replaced since its decode was asked for are not
    /// kept, and the path is decoded afresh when asked again
    #[test]
    fn a_decode_of_a_replaced_file_is_not_kept() {
        use image::DynamicImage;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        DynamicImage::new_rgb8(30, 20).save(&path).unwrap();
        let mut manager = super::LargeImageManager::new();
        let (_, _, generation) = manager.try_decode(&path, None);

        // Replaced before the decode read it
        std::fs::remove_file(&path).unwrap();
        DynamicImage::new_rgb8(40, 20).save(&path).unwrap();
        let read = super::file_stamp(&path);
        let handle = crate::ui::widget::image::Handle::from_rgba(1, 1, vec![0; 4]);
        assert!(!manager.store_decoded_with_generation(
            path.clone(),
            handle,
            None,
            generation,
            read,
        ));
        assert!(manager.get_decoded(&path).is_none());
        assert!(!manager.is_decoding(&path));
        assert!(manager.try_decode(&path, None).0, "asked again");
    }

    /// One gallery asking for another image skips its own older decode, but
    /// not another gallery's
    #[tokio::test]
    async fn a_decode_is_superseded_only_by_its_own_gallery() {
        use image::DynamicImage;
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        DynamicImage::new_rgb8(30, 20).save(&path).unwrap();
        let (one, other) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));

        // Held, so that all three wait for their turn
        let turn = super::DECODE_TURN.acquire().await.unwrap();
        let first = tokio::spawn(super::decode_large_image(
            path.clone(),
            None,
            Arc::clone(&one),
        ));
        let elsewhere = tokio::spawn(super::decode_large_image(
            path.clone(),
            None,
            Arc::clone(&other),
        ));
        let second = tokio::spawn(super::decode_large_image(
            path.clone(),
            None,
            Arc::clone(&one),
        ));
        while one.load(std::sync::atomic::Ordering::Relaxed) < 2
            || other.load(std::sync::atomic::Ordering::Relaxed) < 1
        {
            tokio::task::yield_now().await;
        }
        drop(turn);

        assert!(matches!(first.await.unwrap(), super::Decoded::Superseded));
        assert!(matches!(elsewhere.await.unwrap(), super::Decoded::Done(_)));
        assert!(matches!(second.await.unwrap(), super::Decoded::Done(_)));
    }

    #[test]
    fn available_memory_is_read_from_meminfo() {
        let meminfo = "MemTotal:       65536000 kB\nMemFree:         1000000 kB\nMemAvailable:   42407292 kB\n";
        assert_eq!(super::mem_available_kb(meminfo), Some(42_407_292));
        assert_eq!(super::mem_available_kb("MemTotal: 1 kB\n"), None);
    }
}
