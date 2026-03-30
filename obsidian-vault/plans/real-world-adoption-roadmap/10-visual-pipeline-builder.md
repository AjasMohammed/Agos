---
title: Visual Pipeline Builder
tags:
  - web-ui
  - pipeline
  - no-code
  - plan
  - v3
date: 2026-03-25
status: partial
effort: 8d
priority: low
---

# Phase 10 — Visual Pipeline Builder

> Add a drag-and-drop workflow editor in the web UI where non-technical users can build multi-step agent pipelines by connecting nodes — agents, tools, conditions, and outputs — without writing TOML or CLI commands.

---

## Why This Phase

The research highlights this pattern as a key adoption driver for non-technical users:

> "Adopting the Inkeep model — where a no-code visual builder and a developer SDK are kept in two-way synchronization — allows business teams to adjust logic while developers fine-tune performance."

LangGraph's graph editor is a competitive differentiator for non-technical teams. AgentOS has a full pipeline execution engine (`agentos-pipeline`) but it requires TOML/CLI to configure. A visual builder on top of the existing engine unlocks business users who want to automate workflows without developers.

**Constraint:** This does NOT rebuild the pipeline engine. It is a visual editor that generates the TOML pipeline definition that the existing engine already understands.

---

## Current → Target State

| Area | Current | Target |
|------|---------|--------|
| Pipeline creation | `agentctl pipeline define ...` (CLI, TOML) | Drag-drop visual builder in web UI |
| Pipeline visualization | None | Interactive graph showing nodes + edges |
| Running pipelines | `agentctl pipeline run` | "Run Pipeline" button in UI |
| Pipeline monitoring | None | Real-time node status (green/yellow/red) as pipeline executes |
| TOML ↔ Visual sync | N/A | Visual editor generates TOML; TOML can be imported back to visual |
| Non-technical access | None | Full feature accessible without CLI knowledge |

---

## Architecture

The visual builder is entirely in the web UI. It reads and writes the pipeline TOML format that the existing `agentos-pipeline` crate already understands.

```
Browser (Pipeline Builder UI)
     │
     │  User drags nodes, connects edges
     │  Builder generates PipelineDefinition JSON
     │  POST /api/pipelines  → save to kernel
     │
     ▼
Web Server (Axum)
     │  KernelCommand::PipelineDefine(definition)
     ▼
Kernel → PipelineEngine (existing, unchanged)
     │
     ▼
Pipeline executes with existing agents/tools
```

---

## Detailed Subtasks

### Subtask 10.1 — Pipeline definition data model review

**Read:** `crates/agentos-pipeline/src/lib.rs` and related modules

Before building the UI, document the exact pipeline TOML/JSON schema that the existing engine accepts. Key types to identify:
- `PipelineDefinition` struct (name, description, steps)
- `PipelineStep` struct (id, agent, tool_or_prompt, dependencies, conditions)
- `PipelineCondition` (if-then-else branching)
- `PipelineInput` / `PipelineOutput` (data flow between steps)

The visual builder must generate `PipelineDefinition` JSON that is 1:1 compatible with the existing engine. No engine changes.

---

### Subtask 10.2 — Node types for the visual builder

The visual builder represents each concept as a draggable node:

| Node Type | Color | Description | Configuration |
|-----------|-------|-------------|---------------|
| Start | Green | Pipeline entry point | Input variables |
| Agent | Blue | Run a prompt with an agent | Agent name, model, prompt template |
| Tool | Purple | Execute a specific tool directly | Tool name, static input values |
| Condition | Yellow | Branch based on previous output | Condition expression (contains/equals/regex) |
| Transform | Orange | Extract or format data from output | Field path, regex, template |
| Notify | Teal | Send notification to user | Message template |
| End | Red | Pipeline exit point | Success/failure output |

Edges represent data flow: output of node A becomes input of node B.

---

### Subtask 10.3 — Frontend: canvas with Alpine.js

**File:** `crates/agentos-web/src/templates/pipelines/builder.html` (new)

Use Alpine.js to manage the canvas state. No external graphing library — implement a simple node-edge canvas with SVG arrows and absolute-positioned divs:

```html
<div x-data="pipelineBuilder()" class="pipeline-canvas">
  <!-- Toolbar: node types to drag -->
  <div class="node-palette">
    <div class="node-template" draggable="true" @dragstart="startDrag('agent')">Agent</div>
    <div class="node-template" draggable="true" @dragstart="startDrag('tool')">Tool</div>
    <div class="node-template" draggable="true" @dragstart="startDrag('condition')">Condition</div>
    <div class="node-template" draggable="true" @dragstart="startDrag('notify')">Notify</div>
  </div>

  <!-- Canvas -->
  <div class="canvas-area"
       @drop="dropNode($event)"
       @dragover.prevent>
    <!-- Render nodes -->
    <template x-for="node in nodes" :key="node.id">
      <div class="pipeline-node"
           :class="nodeClass(node)"
           :style="`left:${node.x}px; top:${node.y}px`"
           @mousedown="startDrag(node)">
        <div class="node-header" x-text="node.label"></div>
        <div class="node-ports">
          <div class="port input-port" @mousedown="startEdge(node, 'in')"></div>
          <div class="port output-port" @mousedown="startEdge(node, 'out')"></div>
        </div>
      </div>
    </template>

    <!-- Render SVG edges -->
    <svg class="edge-layer">
      <template x-for="edge in edges">
        <path :d="edgePath(edge)" stroke="#666" stroke-width="2" fill="none"
              marker-end="url(#arrow)"/>
      </template>
    </svg>
  </div>

  <!-- Properties panel: configure selected node -->
  <div class="properties-panel" x-show="selectedNode">
    <template x-if="selectedNode?.node_type === 'agent'">
      <div>
        <label>Agent Name</label>
        <input x-model="selectedNode.config.agent_name"
               hx-get="/api/agents" hx-trigger="focus"
               hx-target="#agent-suggestions">
        <label>Prompt Template</label>
        <textarea x-model="selectedNode.config.prompt"></textarea>
      </div>
    </template>
    <!-- Similar panels for tool, condition, etc. -->
  </div>

  <!-- Actions -->
  <div class="builder-actions">
    <button @click="savePipeline()"
            hx-post="/api/pipelines"
            hx-vals="js:{pipeline: JSON.stringify(toPipelineDefinition())}"
            hx-target="#save-result">Save Pipeline</button>
    <button @click="runPipeline()"
            hx-post="/api/pipelines/run"
            hx-vals="js:{pipeline_id: currentPipelineId}"
            hx-target="#run-status">Run Now</button>
    <button @click="exportToml()">Export TOML</button>
    <button @click="importToml()">Import TOML</button>
  </div>
</div>

<script>
function pipelineBuilder() {
  return {
    nodes: [],
    edges: [],
    selectedNode: null,
    currentPipelineId: null,

    dropNode(event) {
      const type = event.dataTransfer.getData('nodeType');
      const rect = event.currentTarget.getBoundingClientRect();
      this.nodes.push({
        id: crypto.randomUUID(),
        node_type: type,
        label: type.charAt(0).toUpperCase() + type.slice(1),
        x: event.clientX - rect.left,
        y: event.clientY - rect.top,
        config: this.defaultConfig(type),
      });
    },

    toPipelineDefinition() {
      // Convert visual graph to PipelineDefinition JSON
      return {
        name: "My Pipeline",
        steps: this.nodes
          .filter(n => n.node_type !== 'start' && n.node_type !== 'end')
          .map(node => ({
            id: node.id,
            node_type: node.node_type,
            config: node.config,
            depends_on: this.edges
              .filter(e => e.target === node.id)
              .map(e => e.source),
          }))
      };
    },

    edgePath(edge) {
      // SVG bezier curve between source and target ports
      const src = this.nodeOutputPort(edge.source);
      const tgt = this.nodeInputPort(edge.target);
      return `M ${src.x} ${src.y} C ${src.x+60} ${src.y}, ${tgt.x-60} ${tgt.y}, ${tgt.x} ${tgt.y}`;
    },
  };
}
</script>
```

---

### Subtask 10.4 — TOML ↔ JSON bidirectional converter

**File:** `crates/agentos-web/src/handlers/pipeline_ui.rs` (new)

```rust
/// Convert PipelineDefinition to/from the visual builder's JSON format
pub async fn import_pipeline_toml(
    Body(toml_text): Body<String>,
) -> Result<Json<VisualPipelineGraph>> {
    let pipeline: PipelineDefinition = toml::from_str(&toml_text)?;
    Ok(Json(pipeline_to_visual_graph(&pipeline)))
}

pub async fn export_pipeline_toml(
    Json(graph): Json<VisualPipelineGraph>,
) -> Result<String> {
    let pipeline = visual_graph_to_pipeline(&graph)?;
    Ok(toml::to_string_pretty(&pipeline)?)
}
```

A `VisualPipelineGraph` is the JSON format that the Alpine.js frontend uses (nodes with x/y positions + config, plus edges). The `PipelineDefinition` is the existing TOML format. Conversion is lossless for pipeline logic; x/y positions are stored in a `[meta]` section of the TOML.

---

### Subtask 10.5 — Real-time execution visualization

When a pipeline is running, the builder page shows live status updates via SSE:

```html
<div id="pipeline-status"
     hx-ext="sse"
     sse-connect="/api/pipelines/{id}/events"
     hx-swap="none"
     @sse-message="updateNodeStatus($event.detail)">
</div>
```

Alpine.js `updateNodeStatus` updates each node's visual state:
- **Waiting** — grey
- **Running** — blue with pulsing animation
- **Completed** — green with checkmark
- **Failed** — red with X icon

```javascript
updateNodeStatus(event) {
  const data = JSON.parse(event.data);  // {step_id, status, output_preview}
  const node = this.nodes.find(n => n.id === data.step_id);
  if (node) node.status = data.status;
}
```

---

### Subtask 10.6 — Pipeline list page

**File:** `crates/agentos-web/src/templates/pipelines/list.html` (new)

Simple list of saved pipelines with:
- Name, created date, last run, last run status
- "Edit" → opens visual builder
- "Run" → triggers pipeline run
- "Clone" → duplicates pipeline
- "Delete" → confirmation dialog

---

## Files Changed

| File | Change |
|------|--------|
| `crates/agentos-web/src/templates/pipelines/builder.html` | New — visual pipeline builder |
| `crates/agentos-web/src/templates/pipelines/list.html` | New — pipeline list |
| `crates/agentos-web/src/handlers/pipeline_ui.rs` | New — UI handlers + TOML converter |
| `crates/agentos-web/src/router.rs` | Modified — add pipeline UI routes |
| `crates/agentos-web/src/state.rs` | Modified — add pipeline client methods |

No changes to `agentos-pipeline` crate. The builder is purely a UI layer over the existing engine.

---

## Dependencies

- Phase 2 (Web UI Completion) — uses the navigation, base template, and SSE infrastructure
- Existing `agentos-pipeline` crate (already complete)

---

## Test Plan

1. **Node creation** — drag an "Agent" node onto canvas, verify it appears with correct default config
2. **Edge drawing** — drag from output port of node A to input port of node B, verify edge appears
3. **toPipelineDefinition** — build a 3-node pipeline, call `toPipelineDefinition()`, assert output JSON matches `PipelineDefinition` schema
4. **TOML import** — paste a valid pipeline TOML, click import, assert nodes appear on canvas in correct positions
5. **TOML export** — build pipeline visually, export TOML, import back, assert canvas is identical
6. **Run status** — run a saved pipeline, verify node colors update in real time (Waiting → Running → Completed)
7. **Failed step visualization** — run pipeline with a step that fails, verify that step node turns red and subsequent steps remain grey

---

## Verification

```bash
cargo build -p agentos-web
cargo test -p agentos-web -- pipeline

# Manual
agentctl web serve &
# Open http://localhost:8080/pipelines
# Drag nodes, connect, save, run
```

---

## Related

- [[Real World Adoption Roadmap Plan]]
- [[02-web-ui-completion]] — prerequisite: base template and nav from Phase 2
