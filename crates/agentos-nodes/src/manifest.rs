use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeManifest {
    pub node: NodeManifestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeManifestBody {
    pub id: String,
    #[serde(default = "default_version")]
    pub version: u32,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub subcategory: Option<String>,
    #[serde(default)]
    pub icon: String,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub risk_class: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub badge: Option<String>,
    #[serde(default)]
    pub inputs: Vec<NodePort>,
    #[serde(default)]
    pub outputs: Vec<NodePort>,
    #[serde(default)]
    pub credentials: Vec<NodeCredentialRef>,
    #[serde(default)]
    pub properties: Vec<NodeProperty>,
    pub execute: NodeExecute,
}

impl Default for NodeManifestBody {
    fn default() -> Self {
        Self {
            id: String::new(),
            version: 1,
            display_name: String::new(),
            description: String::new(),
            category: "misc".into(),
            subcategory: None,
            icon: String::new(),
            color: "#4a90e2".into(),
            risk_class: String::new(),
            enabled: true,
            badge: None,
            inputs: vec![],
            outputs: vec![],
            credentials: vec![],
            properties: vec![],
            execute: NodeExecute::Builtin {
                name: "noop".into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodePort {
    pub kind: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub multiple: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeCredentialRef {
    #[serde(rename = "type")]
    pub credential_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeProperty {
    pub name: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    #[serde(default)]
    pub required: bool,
    /// Hint: value is a vault secret reference — enables @keyname autocomplete.
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub default: serde_json::Value,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<PropertyOption>,
    #[serde(default)]
    pub type_options: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub display_options: Option<DisplayOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PropertyType {
    #[default]
    String,
    Number,
    Boolean,
    Options,
    MultiOptions,
    Json,
    Code,
    /// Live ecosystem picker: agents | tools | channels | workflows | credentials.
    ResourcePicker,
    /// String with {{var}} template + @keyname completion.
    Template,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PropertyOption {
    pub value: serde_json::Value,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DisplayOptions {
    #[serde(default)]
    pub show: BTreeMap<String, Vec<serde_json::Value>>,
    #[serde(default)]
    pub hide: BTreeMap<String, Vec<serde_json::Value>>,
}

impl DisplayOptions {
    /// Check whether this node property is visible given the current parameter map.
    pub fn is_visible(&self, params: &BTreeMap<String, serde_json::Value>) -> bool {
        for (key, required_values) in &self.show {
            match params.get(key) {
                Some(v) => {
                    if !required_values.contains(v) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        for (key, hidden_values) in &self.hide {
            if let Some(v) = params.get(key) {
                if hidden_values.contains(v) {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeExecute {
    Tool {
        tool_id: String,
        #[serde(default)]
        parameter_mapping: BTreeMap<String, String>,
    },
    Agent {
        #[serde(default = "default_agent_prop")]
        agent_property: String,
        task_template: String,
    },
    Builtin {
        name: String,
    },
    Http {
        url_template: String,
        method: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default)]
        body_template: Option<String>,
    },
    CallWorkflow {
        #[serde(default)]
        workflow_id_property: String,
    },
    KernelCommand {
        command: String,
        parameter_mapping: BTreeMap<String, String>,
    },
}

fn default_version() -> u32 {
    1
}
fn default_category() -> String {
    "misc".into()
}
fn default_color() -> String {
    "#4a90e2".into()
}
fn default_true() -> bool {
    true
}
fn default_agent_prop() -> String {
    "agent_name".into()
}
