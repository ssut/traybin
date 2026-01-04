//! Image indexing and vector search using LanceDB and FastEmbed

use anyhow::{Context, Result};
use arrow_array::{
    Array, FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
    UInt64Array, types::Float32Type,
};
use arrow_schema::{DataType, Field, Schema};
use crossbeam_channel::Sender;
use fastembed::{
    EmbeddingModel, ImageEmbedding, ImageEmbeddingModel, ImageInitOptions, InitOptions,
    TextEmbedding,
};
use futures::stream::TryStreamExt;
use lancedb::Connection;
use lancedb::query::ExecutableQuery;
use log::{error, info, warn};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::AppMessage;

/// Image file extensions to index
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "avif"];

/// Configuration for the indexer
#[derive(Clone)]
pub struct IndexConfig {
    pub db_path: PathBuf,
    pub cpu_mode: CpuMode,
    pub screenshot_dir: PathBuf,
}

/// CPU mode for indexing
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CpuMode {
    /// Balanced mode: batch size 32, 50ms delay
    Normal,
    /// Fast mode: batch size 256, no delays
    Fast,
}

impl CpuMode {
    fn batch_size(&self) -> usize {
        match self {
            CpuMode::Normal => 8,
            CpuMode::Fast => 32,
        }
    }

    fn delay_ms(&self) -> u64 {
        match self {
            CpuMode::Normal => 25,
            CpuMode::Fast => 0,
        }
    }
}

/// Index statistics
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IndexStats {
    pub indexed_count: usize,
    pub total_size_mb: f64,
    pub last_updated: SystemTime,
}

impl Default for IndexStats {
    fn default() -> Self {
        Self {
            indexed_count: 0,
            total_size_mb: 0.0,
            last_updated: SystemTime::now(),
        }
    }
}

/// Main indexer state
pub struct IndexerState {
    config: IndexConfig,
    db: Option<Connection>,
    image_model: Option<Arc<Mutex<ImageEmbedding>>>,
    text_model: Option<Arc<Mutex<TextEmbedding>>>,
    indexed_files: Arc<Mutex<HashSet<PathBuf>>>,
    message_tx: Sender<AppMessage>,
}

impl IndexerState {
    /// Create new indexer state
    pub fn new(config: IndexConfig, message_tx: Sender<AppMessage>) -> Self {
        Self {
            config,
            db: None,
            image_model: None,
            text_model: None,
            indexed_files: Arc::new(Mutex::new(HashSet::new())),
            message_tx,
        }
    }

    /// Check if models are ready
    fn models_ready(&self) -> bool {
        self.image_model.is_some() && self.text_model.is_some()
    }

    /// Download embedding models with progress tracking
    fn download_models(message_tx: Sender<AppMessage>) -> Result<(ImageEmbedding, TextEmbedding)> {
        info!("Starting model download...");

        // Set FastEmbed cache directory to appdata
        let cache_dir = crate::settings::Settings::config_path()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let cache_dir = cache_dir.join(".fastembed_cache");

        // Create cache directory if it doesn't exist
        if let Err(e) = fs::create_dir_all(&cache_dir) {
            warn!("Failed to create cache directory: {}", e);
        }

        // Set environment variable for FastEmbed
        info!("FastEmbed cache directory: {:?}", cache_dir);

        let _ = message_tx.send(AppMessage::ModelDownloadProgress(1, 2, "Loading Vision Model".into()));
        let image_model = ImageEmbedding::try_new(
            ImageInitOptions::new(ImageEmbeddingModel::NomicEmbedVisionV15)
                .with_cache_dir(PathBuf::from(cache_dir.to_str().unwrap()))
                .with_show_download_progress(false),
        )
        .context("Failed to load vision model")?;
        info!("Vision model loaded");

        let _ = message_tx.send(AppMessage::ModelDownloadProgress(2, 2, "Loading Text Model".into()));
        let text_model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
                .with_cache_dir(PathBuf::from(cache_dir.to_str().unwrap()))
                .with_show_download_progress(false),
        )
        .context("Failed to load text model")?;
        info!("Text model loaded");

        let _ = message_tx.send(AppMessage::ModelDownloadCompleted);
        Ok((image_model, text_model))
    }

    /// Create database schema
    fn create_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("file_path", DataType::Utf8, false),
            Field::new("file_size", DataType::UInt64, false),
            Field::new("modified_time", DataType::Int64, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), 768),
                true,
            ),
        ]))
    }

    /// Open or create database
    async fn open_or_create_db(db_path: &Path) -> Result<Connection> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        match lancedb::connect(db_path.to_str().unwrap()).execute().await {
            Ok(db) => {
                info!("Connected to database: {:?}", db_path);
                Ok(db)
            }
            Err(e) => {
                warn!("Failed to open database, recreating: {}", e);
                // Delete corrupt DB
                if db_path.exists() {
                    fs::remove_dir_all(db_path)?;
                }
                // Recreate
                lancedb::connect(db_path.to_str().unwrap())
                    .execute()
                    .await
                    .context("Failed to create new database")
            }
        }
    }

    /// Load indexed files from database
    async fn load_indexed_files(&mut self) -> Result<()> {
        if self.db.is_none() {
            return Ok(());
        }

        let db = self.db.as_ref().unwrap();

        // Check if table exists
        let table_names = db.table_names().execute().await?;
        if !table_names.contains(&"images".to_string()) {
            info!("Images table doesn't exist yet, starting fresh");
            return Ok(());
        }

        let table = db.open_table("images").execute().await?;

        // Query all file paths
        let mut results = table.query().execute().await?;

        let mut indexed = self.indexed_files.lock();

        while let Some(batch) = results.try_next().await? {
            if let Some(path_col) = batch.column_by_name("file_path") {
                let path_array: &StringArray =
                    path_col.as_any().downcast_ref::<StringArray>().unwrap();
                for i in 0..path_array.len() {
                    if !path_array.is_null(i) {
                        let path_str = path_array.value(i);
                        indexed.insert(PathBuf::from(path_str));
                    }
                }
            }
        }

        info!("Loaded {} indexed files from database", indexed.len());
        Ok(())
    }

    /// Check if a file should be indexed
    #[allow(dead_code)]
    fn should_index(&self, path: &Path) -> bool {
        let indexed = self.indexed_files.lock();

        if !indexed.contains(path) {
            return true; // New file
        }

        // For now, skip files that are already indexed
        // TODO: Check modification time for updates
        false
    }

    /// Check if path is an image file
    fn is_image_file(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|&e| e.eq_ignore_ascii_case(ext))
            })
    }

    /// Collect files to index
    fn collect_files_to_index(&self, force_all: bool) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        fn visit_dirs(
            dir: &Path,
            files: &mut Vec<PathBuf>,
            should_check: bool,
            indexed_set: &HashSet<PathBuf>,
        ) -> Result<()> {
            if dir.is_dir() {
                for entry in fs::read_dir(dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_dir() {
                        // Recursively visit subdirectories
                        visit_dirs(&path, files, should_check, indexed_set)?;
                    } else if IndexerState::is_image_file(&path) {
                        if !should_check || !indexed_set.contains(&path) {
                            files.push(path);
                        }
                    }
                }
            }
            Ok(())
        }

        let indexed = self.indexed_files.lock();
        visit_dirs(
            &self.config.screenshot_dir,
            &mut files,
            !force_all,
            &indexed,
        )?;

        info!("Found {} files to index", files.len());
        Ok(files)
    }

    /// Insert embeddings into database
    async fn insert_embeddings(
        &mut self,
        paths: &[PathBuf],
        embeddings: Vec<Vec<f32>>,
    ) -> Result<()> {
        if paths.len() != embeddings.len() {
            anyhow::bail!("Mismatch between paths and embeddings count");
        }

        let db = self.db.as_ref().unwrap();

        // Prepare data
        let mut file_paths = Vec::new();
        let mut file_sizes = Vec::new();
        let mut modified_times = Vec::new();
        let mut vectors = Vec::new();

        for (path, embedding) in paths.iter().zip(embeddings.iter()) {
            if let Ok(metadata) = fs::metadata(path) {
                file_paths.push(path.to_str().unwrap().to_string());
                file_sizes.push(metadata.len());

                let mtime = metadata
                    .modified()
                    .unwrap_or(SystemTime::now())
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                modified_times.push(mtime);

                // Convert Vec<f32> to Vec<Option<f32>> for Arrow
                let embedding_opts: Vec<Option<f32>> = embedding.iter().map(|&v| Some(v)).collect();
                vectors.push(Some(embedding_opts));
            }
        }

        if file_paths.is_empty() {
            return Ok(());
        }

        // Create Arrow arrays
        let schema = Self::create_schema();

        let path_array = StringArray::from(file_paths.clone());
        let size_array = UInt64Array::from(file_sizes);
        let mtime_array = Int64Array::from(modified_times);
        let vector_array =
            FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(vectors.into_iter(), 768);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(path_array),
                Arc::new(size_array),
                Arc::new(mtime_array),
                Arc::new(vector_array),
            ],
        )?;

        // Check if table exists
        let table_names = db.table_names().execute().await?;
        if table_names.contains(&"images".to_string()) {
            // Append to existing table
            let table = db.open_table("images").execute().await?;
            let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema.clone());
            table.add(Box::new(batches)).execute().await?;
        } else {
            // Create new table
            let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema.clone());
            db.create_table("images", Box::new(batches))
                .execute()
                .await?;
        }

        // Update indexed files set
        {
            let mut indexed = self.indexed_files.lock();
            for path in paths {
                indexed.insert(path.clone());
            }
        }

        Ok(())
    }

    /// Index a batch of files
    async fn index_batch(
        &mut self,
        files: Vec<PathBuf>,
        indexed_count: &mut usize,
        total: usize,
    ) -> Result<()> {
        let batch_size = self.config.cpu_mode.batch_size();
        let delay_ms = self.config.cpu_mode.delay_ms();

        for (chunk_idx, chunk) in files.chunks(batch_size).enumerate() {
            let file_path_strings: Vec<String> = chunk
                .iter()
                .filter_map(|p| p.to_str().map(|s| s.to_string()))
                .collect();

            info!("Processing batch {}: {} files", chunk_idx, chunk.len());

            if file_path_strings.is_empty() {
                warn!("Batch {} has no valid file paths, skipping", chunk_idx);
                continue;
            }

            // Get current file name for progress
            let current_file = chunk[0]
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Embed images (blocking operation)
            let image_model = self.image_model.as_ref().unwrap().clone();
            let embeddings_result = tokio::task::spawn_blocking(move || {
                let file_path_refs: Vec<&str> =
                    file_path_strings.iter().map(|s| s.as_str()).collect();
                let mut model = image_model.lock();
                model.embed(file_path_refs, None)
            })
            .await??;

            // Convert embeddings to Vec<Vec<f32>>
            let embeddings: Vec<Vec<f32>> = embeddings_result.into_iter().collect();

            info!("Batch {}: Got {} embeddings for {} files", chunk_idx, embeddings.len(), chunk.len());

            // Insert into database (only files with valid embeddings)
            let num_inserted = embeddings.len().min(chunk.len());
            self.insert_embeddings(&chunk[..num_inserted], embeddings).await?;

            *indexed_count += num_inserted;
            info!("Batch {}: Successfully indexed {} files (total: {}/{})", chunk_idx, num_inserted, *indexed_count, total);

            // Send progress update
            let _ = self.message_tx.send(AppMessage::IndexProgress(
                *indexed_count,
                total,
                current_file,
            ));

            // Throttle if needed
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
        }

        Ok(())
    }

    /// Run the indexing process
    pub async fn run_indexing(&mut self, force_all: bool) -> Result<()> {
        info!("Starting indexing process (force_all: {})", force_all);

        // Open database
        self.db = Some(Self::open_or_create_db(&self.config.db_path).await?);

        // Load existing indexed files
        if !force_all {
            self.load_indexed_files().await?;
        }

        // Collect files to index
        let files = self.collect_files_to_index(force_all)?;
        let total = files.len();

        info!("Found {} files to index", total);

        if total == 0 {
            info!("No files to index");
            let _ = self.message_tx.send(AppMessage::IndexCompleted(0));
            return Ok(());
        }

        // Send start message
        let _ = self.message_tx.send(AppMessage::IndexStarted(total));

        // Index files
        let mut indexed_count = 0;
        match self.index_batch(files, &mut indexed_count, total).await {
            Ok(_) => {
                info!("Successfully indexed {} out of {} files", indexed_count, total);
            }
            Err(e) => {
                error!("Error during indexing: {}. Indexed {} files before error.", e, indexed_count);
                // Continue and send the count of files that were successfully indexed
            }
        }

        // Send completion message with count
        let _ = self
            .message_tx
            .send(AppMessage::IndexCompleted(indexed_count));
        info!("Indexing completed: {} files processed", indexed_count);

        Ok(())
    }
}

/// Start indexing in a background thread
/// If prewarmed models are provided, they will be used instead of loading fresh models
pub fn start_indexing(
    config: IndexConfig,
    message_tx: Sender<AppMessage>,
    force_all: bool,
    prewarmed_vision: Option<Arc<Mutex<ImageEmbedding>>>,
    prewarmed_text: Option<Arc<Mutex<TextEmbedding>>>,
) {
    std::thread::spawn(move || {
        info!("Indexing thread started");

        // Create Tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut state = IndexerState::new(config, message_tx.clone());

            // Use prewarmed models if provided, otherwise download
            if let (Some(vision), Some(text)) = (prewarmed_vision, prewarmed_text) {
                info!("Using prewarmed models for indexing (no loading needed)");
                state.image_model = Some(vision);
                state.text_model = Some(text);
            } else if !state.models_ready() {
                info!("Loading models for indexing...");
                let download_tx = message_tx.clone();
                match tokio::task::spawn_blocking(move || {
                    IndexerState::download_models(download_tx)
                })
                .await
                {
                    Ok(Ok((img_model, txt_model))) => {
                        state.image_model = Some(Arc::new(Mutex::new(img_model)));
                        state.text_model = Some(Arc::new(Mutex::new(txt_model)));
                    }
                    Ok(Err(e)) => {
                        error!("Model download failed: {}", e);
                        let _ = message_tx.send(AppMessage::ModelDownloadFailed(e.to_string()));
                        return;
                    }
                    Err(e) => {
                        error!("Model download task failed: {}", e);
                        let _ = message_tx.send(AppMessage::ModelDownloadFailed(e.to_string()));
                        return;
                    }
                }
            }

            // Run indexing
            match state.run_indexing(force_all).await {
                Ok(_) => {
                    info!("Indexing completed successfully");
                }
                Err(e) => {
                    error!("Indexing failed: {}", e);
                    let _ = message_tx.send(AppMessage::IndexFailed(e.to_string()));
                }
            }
        });
    });
}

/// Index a single file (for auto-indexing new screenshots)
#[allow(dead_code)]
pub fn index_single_file(_path: PathBuf, _config: IndexConfig, _message_tx: Sender<AppMessage>) {
    // TODO: Implement single file indexing
    // For now, this is a placeholder - full re-indexing will pick up new files
}

/// Search for images by text query
pub fn search_images(
    query: String,
    config: IndexConfig,
    text_model: Arc<Mutex<TextEmbedding>>,
    message_tx: Sender<AppMessage>,
    limit: usize,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            match search_images_impl(query, config, text_model, limit).await {
                Ok(paths) => {
                    let _ = message_tx.send(AppMessage::SearchResults(paths));
                }
                Err(e) => {
                    error!("Search failed: {}", e);
                    let _ = message_tx.send(AppMessage::SearchResults(Vec::new()));
                }
            }
        });
    });
}

/// Internal search implementation
async fn search_images_impl(
    query: String,
    config: IndexConfig,
    text_model: Arc<Mutex<TextEmbedding>>,
    limit: usize,
) -> Result<Vec<PathBuf>> {
    info!("Searching for: {}", query);

    // Embed text query
    let query_string_vec = vec![query.clone()];
    let query_embedding_result = tokio::task::spawn_blocking(move || {
        let query_strs: Vec<&str> = query_string_vec.iter().map(|s| s.as_str()).collect();
        let mut model = text_model.lock();
        model.embed(query_strs, None)
    })
    .await??;

    if query_embedding_result.is_empty() {
        return Ok(Vec::new());
    }

    // Convert embedding to Vec<f32>
    let query_vec: Vec<f32> = query_embedding_result[0].clone().into_iter().collect();

    // Open database
    let db = IndexerState::open_or_create_db(&config.db_path).await?;

    // Check if table exists
    let table_names = db.table_names().execute().await?;
    if !table_names.contains(&"images".to_string()) {
        return Ok(Vec::new());
    }

    let table = db.open_table("images").execute().await?;

    // Vector search
    let mut results = table
        .query()
        .nearest_to(query_vec.as_slice())?
        .execute()
        .await?;

    // Extract file paths
    let mut paths = Vec::new();
    while let Some(batch) = results.try_next().await? {
        if let Some(path_col) = batch.column_by_name("file_path") {
            let path_array: &StringArray = path_col.as_any().downcast_ref::<StringArray>().unwrap();
            for i in 0..path_array.len() {
                if paths.len() >= limit {
                    break;
                }
                if !path_array.is_null(i) {
                    let path_str = path_array.value(i);
                    let path = PathBuf::from(path_str);
                    if path.exists() {
                        paths.push(path);
                    }
                }
            }
        }
        if paths.len() >= limit {
            break;
        }
    }

    info!("Found {} matching images", paths.len());
    Ok(paths)
}

/// Get index statistics
#[allow(dead_code)]
pub async fn get_index_stats(config: &IndexConfig) -> Result<IndexStats> {
    let db = IndexerState::open_or_create_db(&config.db_path).await?;

    let table_names = db.table_names().execute().await?;
    if !table_names.contains(&"images".to_string()) {
        return Ok(IndexStats::default());
    }

    let table = db.open_table("images").execute().await?;
    let count = table.count_rows(None).await?;

    // Calculate total size
    let mut total_size: u64 = 0;
    let mut results = table.query().execute().await?;

    while let Some(batch) = results.try_next().await? {
        if let Some(size_col) = batch.column_by_name("file_size") {
            let size_array: &UInt64Array = size_col.as_any().downcast_ref::<UInt64Array>().unwrap();
            for i in 0..size_array.len() {
                if !size_array.is_null(i) {
                    total_size += size_array.value(i);
                }
            }
        }
    }

    Ok(IndexStats {
        indexed_count: count,
        total_size_mb: total_size as f64 / (1024.0 * 1024.0),
        last_updated: SystemTime::now(),
    })
}

/// Get total indexed count from database (synchronous wrapper)
pub fn get_indexed_count(config: &IndexConfig) -> Result<usize> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let db = IndexerState::open_or_create_db(&config.db_path).await?;

        let table_names = db.table_names().execute().await?;
        if !table_names.contains(&"images".to_string()) {
            return Ok(0);
        }

        let table = db.open_table("images").execute().await?;
        let count = table.count_rows(None).await?;

        Ok(count)
    })
}

/// Remove a file from the index by path (cleanup for deleted files)
pub fn remove_from_index(path: PathBuf, config: IndexConfig) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            match remove_from_index_impl(path.clone(), config).await {
                Ok(_) => {
                    info!("Removed {:?} from vector index", path);
                }
                Err(e) => {
                    warn!("Failed to remove {:?} from index: {}", path, e);
                }
            }
        });
    });
}

/// Remove implementation (async)
async fn remove_from_index_impl(path: PathBuf, config: IndexConfig) -> Result<()> {
    let db = IndexerState::open_or_create_db(&config.db_path).await?;

    let table_names = db.table_names().execute().await?;
    if !table_names.contains(&"images".to_string()) {
        // Table doesn't exist, nothing to remove
        return Ok(());
    }

    let table = db.open_table("images").execute().await?;

    // Delete rows where path matches
    let path_str = path.to_string_lossy().to_string();
    table
        .delete(&format!("path = '{}'", path_str))
        .await?;

    info!("Deleted index entry for: {:?}", path);
    Ok(())
}
