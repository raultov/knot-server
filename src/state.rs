use std::sync::{Arc, Mutex};

use knot::db::graph::GraphDb;
use knot::db::vector::VectorDb;
use knot::pipeline::embed::Embedder;

use crate::models::IndexJob;
use crate::registry::Registry;

pub struct AppState {
    pub vector_db: Arc<VectorDb>,
    pub graph_db: Arc<GraphDb>,
    pub embedder: Arc<Mutex<Embedder>>,
    pub workspace_dir: String,
    pub registry: Arc<Mutex<Registry>>,
    pub job_tx: tokio::sync::mpsc::Sender<IndexJob>,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: String,
    pub embed_dim: u64,
    pub rayon_threads: Option<usize>,
    pub batch_size: usize,
    pub ingest_concurrency: usize,
    pub start_time: std::time::Instant,
}
