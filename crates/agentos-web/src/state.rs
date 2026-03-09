use agentos_kernel::Kernel;
use minijinja::Environment;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub templates: Arc<Environment<'static>>,
}
