use agentos_nodes::{
    NodeContributor, NodeExecute, NodeManifest, NodeManifestBody, NodePort, NodeProperty,
    PropertyType,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Reads saved workflow specs from `<data_dir>/workflows/` and exposes each
/// as a "Call Workflow" node in the palette. Returns empty if the directory
/// does not exist (workflows dir is created when the first workflow is saved).
pub struct WorkflowNodeContributor {
    workflows_dir: Arc<RwLock<PathBuf>>,
}

impl WorkflowNodeContributor {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            workflows_dir: Arc::new(RwLock::new(data_dir.join("workflows"))),
        }
    }
}

#[async_trait::async_trait]
impl NodeContributor for WorkflowNodeContributor {
    fn category_prefix(&self) -> &str {
        "workflow"
    }

    fn category_display_name(&self) -> &str {
        "Workflows"
    }

    fn sort_order(&self) -> u32 {
        60
    }

    async fn contribute_nodes(&self) -> Vec<NodeManifest> {
        let dir = self.workflows_dir.read().await.clone();
        if !dir.exists() {
            return vec![];
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return vec![],
        };
        entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "json")
                    .unwrap_or(false)
            })
            .filter_map(|e| {
                let path = e.path();
                let stem = path.file_stem()?.to_str()?.to_string();
                let raw = std::fs::read_to_string(&path).ok()?;
                let meta: serde_json::Value = serde_json::from_str(&raw).ok()?;
                let name = meta
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&stem)
                    .to_string();
                Some(NodeManifest {
                    node: NodeManifestBody {
                        id: format!("workflow.{}", stem),
                        display_name: format!("Call: {}", name),
                        description: format!("Execute the '{}' sub-workflow.", name),
                        category: "workflows".into(),
                        icon: "git-branch".into(),
                        color: "#f97316".into(),
                        risk_class: "exec_capable".into(),
                        inputs: vec![NodePort {
                            kind: "main".into(),
                            required: true,
                            ..Default::default()
                        }],
                        outputs: vec![NodePort {
                            kind: "main".into(),
                            ..Default::default()
                        }],
                        properties: vec![NodeProperty {
                            name: "input".into(),
                            display_name: "Input Data".into(),
                            property_type: PropertyType::Json,
                            description: "Data to pass to the sub-workflow.".into(),
                            ..Default::default()
                        }],
                        execute: NodeExecute::CallWorkflow {
                            workflow_id_property: stem,
                        },
                        ..Default::default()
                    },
                })
            })
            .collect()
    }
}
