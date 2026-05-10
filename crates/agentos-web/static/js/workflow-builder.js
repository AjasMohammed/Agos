/* Workflow Builder — Alpine.js controller bridging Drawflow ↔ WorkflowSpec
 *
 * WorkflowNode on-wire shape (matches Rust struct exactly):
 *   { id, name, type, type_version, position:[x,y], parameters, credentials, disabled, notes }
 */

function workflowBuilder(initialSpec) {
  return {
    /* ── state ─────────────────────────────────────────── */
    spec: initialSpec,        // WorkflowSpec JSON (source of truth)
    df: null,                 // Drawflow instance
    selectedNodeId: null,     // WorkflowSpec node id (string)
    dirty: false,
    saving: false,
    running: false,
    runModalOpen: false,
    runInput: '',
    runEvents: [],
    runEventSource: null,
    nodeRunStatus: {},        // spec_id → 'pending'|'running'|'complete'|'failed'|'skipped'
    paletteFilter: '',
    /* dfId ↔ specId maps */
    dfToSpec: {},             // dfId (number) → specId (string)
    specToDf: {},             // specId (string) → dfId (number)

    /* ── lifecycle ──────────────────────────────────────── */
    init() {
      const el = this.$refs.drawflow;
      this.df = new Drawflow(el);
      this.df.reroute = true;
      this.df.zoom_value = 1;
      this.df.start();
      this._bindEvents();
      this._hydrate();
    },

    destroy() {
      if (this.runEventSource) this.runEventSource.close();
    },

    /* ── Drawflow event binding ─────────────────────────── */
    _bindEvents() {
      this.df.on('nodeRemoved',    id => this._onDfNodeRemoved(id));
      this.df.on('nodeSelected',   id => this._onDfNodeSelected(id));
      this.df.on('nodeUnselected', ()  => this._onDfNodeUnselected());
      this.df.on('connectionCreated', info => this._onDfConnCreated(info));
      this.df.on('connectionRemoved', info => this._onDfConnRemoved(info));
      this.df.on('nodeMoved',      id => this._onDfNodeMoved(id));
    },

    /* ── hydrate canvas from spec ───────────────────────── */
    _hydrate() {
      if (!this.spec || !this.spec.nodes) return;
      for (const node of this.spec.nodes) {
        this._addDfNode(node);
      }
      if (!this.spec.connections) return;
      for (const [srcId, outs] of Object.entries(this.spec.connections)) {
        for (const indices of Object.values(outs)) {
          for (const targets of indices) {
            for (const t of targets) {
              const srcDf = this.specToDf[srcId];
              const dstDf = this.specToDf[t.node];
              if (srcDf != null && dstDf != null) {
                this.df.addConnection(srcDf, dstDf, 'output_1', 'input_1');
              }
            }
          }
        }
      }
    },

    /* ── add a node to Drawflow canvas ─────────────────── */
    _addDfNode(specNode) {
      const html = this._renderNodeCard(specNode);
      const x = specNode.position?.[0] ?? 100;
      const y = specNode.position?.[1] ?? 100;
      const dfId = this.df.addNode(
        specNode.id,
        1,            // inputs
        1,            // outputs
        x, y,
        'df-node',
        { spec_id: specNode.id, node_type: specNode.type },
        html,
        false
      );
      this.dfToSpec[dfId] = specNode.id;
      this.specToDf[specNode.id] = dfId;
    },

    /* ── render the inner HTML of a node card ───────────── */
    _renderNodeCard(specNode) {
      const label = specNode.name || specNode.type || 'Node';
      const color = this._nodeColor(specNode.type);
      return `<div class="wb-node-card" style="--node-color:${color}">
        <div class="wb-node-header">
          <span class="wb-node-title" title="${label}">${label}</span>
        </div>
        <div class="df-status-badge" data-status="pending" data-specid="${specNode.id}"></div>
      </div>`;
    },

    _nodeColor(type) {
      const map = {
        'start':         '#22c55e',
        'end':           '#ef4444',
        'agent-task':    '#6366f1',
        'tool-run':      '#f59e0b',
        'http-request':  '#0ea5e9',
        'memory':        '#8b5cf6',
        'channel-send':  '#ec4899',
        'mcp-call':      '#14b8a6',
        'call-workflow': '#f97316',
        'if-condition':  '#84cc16',
        'code':          '#64748b',
      };
      return map[type] ?? '#4a90e2';
    },

    /* ── Drawflow event handlers ────────────────────────── */
    _onDfNodeRemoved(dfId) {
      const specId = this.dfToSpec[dfId];
      if (!specId) return;
      delete this.dfToSpec[dfId];
      delete this.specToDf[specId];
      this.spec.nodes = (this.spec.nodes || []).filter(n => n.id !== specId);
      this._removeConnectionsFor(specId);
      if (this.selectedNodeId === specId) {
        this.selectedNodeId = null;
        document.getElementById('wb-inspector-content').innerHTML =
          '<p class="muted">Select a node to edit its properties.</p>';
      }
      this.dirty = true;
    },

    _onDfNodeSelected(dfId) {
      this.selectedNodeId = this.dfToSpec[dfId];
      if (this.selectedNodeId) this._loadProperties(this.selectedNodeId);
    },

    _onDfNodeUnselected() {
      this.selectedNodeId = null;
    },

    _onDfConnCreated(info) {
      const srcSpecId = this.dfToSpec[info.output_id];
      const dstSpecId = this.dfToSpec[info.input_id];
      if (!srcSpecId || !dstSpecId) return;
      if (!this.spec.connections) this.spec.connections = {};
      if (!this.spec.connections[srcSpecId]) this.spec.connections[srcSpecId] = {};
      if (!this.spec.connections[srcSpecId]['main']) this.spec.connections[srcSpecId]['main'] = [[]];
      this.spec.connections[srcSpecId]['main'][0].push({ node: dstSpecId, type: 'main', index: 0 });
      this.dirty = true;
    },

    _onDfConnRemoved(info) {
      const srcSpecId = this.dfToSpec[info.output_id];
      const dstSpecId = this.dfToSpec[info.input_id];
      if (!srcSpecId || !dstSpecId) return;
      const bucket = this.spec.connections?.[srcSpecId]?.['main']?.[0];
      if (!bucket) return;
      const idx = bucket.findIndex(t => t.node === dstSpecId);
      if (idx !== -1) bucket.splice(idx, 1);
      this.dirty = true;
    },

    _onDfNodeMoved(dfId) {
      const specId = this.dfToSpec[dfId];
      if (!specId) return;
      const data = this.df.getNodeFromId(dfId);
      const node = (this.spec.nodes || []).find(n => n.id === specId);
      if (node && data) {
        node.position = [Math.round(data.pos_x), Math.round(data.pos_y)];
        this.dirty = true;
      }
    },

    _removeConnectionsFor(specId) {
      if (!this.spec.connections) return;
      delete this.spec.connections[specId];
      for (const src of Object.values(this.spec.connections)) {
        for (const port of Object.values(src)) {
          for (const bucket of port) {
            const idx = bucket.findIndex(t => t.node === specId);
            if (idx !== -1) bucket.splice(idx, 1);
          }
        }
      }
    },

    /* ── drag-drop from palette ─────────────────────────── */
    onDrop(event) {
      event.preventDefault();
      const nodeType = event.dataTransfer.getData('node_type');
      if (!nodeType) return;
      const nodeLabel = event.dataTransfer.getData('node_label') || nodeType;
      const rect = this.$refs.drawflow.getBoundingClientRect();
      const x = Math.round((event.clientX - rect.left - this.df.canvas_x) / this.df.zoom);
      const y = Math.round((event.clientY - rect.top  - this.df.canvas_y) / this.df.zoom);
      this.addNode(nodeType, nodeLabel, x, y);
    },

    onDragover(event) { event.preventDefault(); },

    /* ── add a new node to spec + canvas ────────────────── */
    addNode(nodeType, name, x = 200, y = 200) {
      const id = `${nodeType}-${Date.now()}`;
      const specNode = {
        id,
        name,
        type: nodeType,
        type_version: 1,
        position: [x, y],
        parameters: {},
        credentials: {},
        disabled: false,
        notes: null,
      };
      if (!this.spec.nodes) this.spec.nodes = [];
      this.spec.nodes.push(specNode);
      this._addDfNode(specNode);
      this.dirty = true;
    },

    /* ── property panel (HTMX-driven) ──────────────────── */
    _loadProperties(specId) {
      const node = (this.spec.nodes || []).find(n => n.id === specId);
      if (!node) return;
      htmx.ajax('POST', '/api/workflows/node-properties', {
        target: '#wb-inspector-content',
        swap: 'innerHTML',
        values: {
          node_type: node.type,
          node_id: specId,
          parameters: JSON.stringify(node.parameters || {}),
        },
      });
    },

    /* called by property panel partials when a field changes */
    updateParam(specId, key, value) {
      const node = (this.spec.nodes || []).find(n => n.id === specId);
      if (!node) return;
      node.parameters = node.parameters || {};
      node.parameters[key] = value;
      if (key === '_name') {
        node.name = value;
        this._refreshNodeLabel(specId, value);
      }
      this.dirty = true;
    },

    _refreshNodeLabel(specId, newLabel) {
      const dfId = this.specToDf[specId];
      if (dfId == null) return;
      const el = document.querySelector(`#node-${dfId} .wb-node-title`);
      if (el) el.textContent = newLabel;
    },

    /* ── palette search ─────────────────────────────────── */
    filterPalette() {
      const q = this.paletteFilter.toLowerCase();
      document.querySelectorAll('.wb-palette-item').forEach(el => {
        const text = (el.dataset.label ?? '').toLowerCase();
        el.style.display = (!q || text.includes(q)) ? '' : 'none';
      });
    },

    /* ── canvas controls ────────────────────────────────── */
    zoomIn()  { this.df.zoom_in();    },
    zoomOut() { this.df.zoom_out();   },
    zoomFit() { this.df.zoom_reset(); },

    /* ── save ───────────────────────────────────────────── */
    /* ── CSRF helper ────────────────────────────────────────────────────── */
    _csrfToken() {
      return document.querySelector('meta[name="csrf-token"]')?.content;
    },

    async save() {
      this.saving = true;
      try {
        const isNew = !this.spec.id;
        const url    = isNew ? '/api/workflows'                  : `/api/workflows/${this.spec.id}`;
        const method = isNew ? 'POST'                            : 'PUT';
        const resp = await fetch(url, {
          method,
          headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': this._csrfToken() },
          body: JSON.stringify(this.spec),
        });
        if (!resp.ok) throw new Error(await resp.text());
        const saved = await resp.json();
        this.spec.id = saved.id;
        this.dirty = false;
        if (isNew && saved.id) {
          window.history.replaceState({}, '', `/workflows/${saved.id}/edit`);
        }
      } catch (e) {
        alert('Save failed: ' + e.message);
      } finally {
        this.saving = false;
      }
    },

    /* ── run ────────────────────────────────────────────── */
    async openRunModal() {
      if (this.dirty) await this.save();
      this.runModalOpen = true;
      this.runEvents = [];
    },

    closeRunModal() {
      this.runModalOpen = false;
      if (this.runEventSource) { this.runEventSource.close(); this.runEventSource = null; }
    },

    async run() {
      this.running = true;
      this.runEvents = [];
      this.nodeRunStatus = {};
      try {
        const resp = await fetch(`/api/workflows/${this.spec.id}/run`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': this._csrfToken() },
          body: JSON.stringify({ input: this.runInput }),
        });
        if (!resp.ok) throw new Error(await resp.text());
        const { run_id } = await resp.json();
        this._streamRun(run_id);
      } catch (e) {
        this.runEvents.push({ type: 'error', message: e.message });
        this.running = false;
      }
    },

    _streamRun(runId) {
      const es = new EventSource(`/api/workflows/runs/${runId}/events`);
      this.runEventSource = es;
      es.onmessage = (e) => {
        const evt = JSON.parse(e.data);
        this.runEvents.push(evt);
        if (evt.node_id) {
          this.nodeRunStatus[evt.node_id] = evt.status;
          this._setNodeStatus(evt.node_id, evt.status);
        }
        if (evt.type === 'complete' || evt.type === 'error') {
          this.running = false;
          es.close();
          this.runEventSource = null;
        }
      };
      es.onerror = () => {
        this.running = false;
        es.close();
        this.runEventSource = null;
      };
    },

    _setNodeStatus(specId, status) {
      const el = document.querySelector(`.df-status-badge[data-specid="${specId}"]`);
      if (el) el.dataset.status = status;
    },

    /* ── export ─────────────────────────────────────────── */
    exportJson() {
      const blob = new Blob([JSON.stringify(this.spec, null, 2)], { type: 'application/json' });
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = `${this.spec.name || 'workflow'}.json`;
      a.click();
      URL.revokeObjectURL(a.href);
    },
  };
}
