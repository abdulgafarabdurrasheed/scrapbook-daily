use crate::config::Config;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicUsize},
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub client: reqwest::Client,
    pub db_path: Arc<PathBuf>,
    pub ai_key_rotation: Arc<AtomicUsize>,
}
