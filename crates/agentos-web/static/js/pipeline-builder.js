// Alpine.js component for the pipeline visual builder.
// The current backend executes start, agent, tool, and end nodes, so the UI
// focuses on making those steps fast to compose and easy to inspect.
function pipelineBuilder() {
    return {
        graph: {
            name: '',
            version: '1.0.0',
            description: '',
            max_cost_usd: null,
            max_wall_time_minutes: null,
            nodes: [],
            edges: []
        },
        agentNames: [],
        toolNames: [],
        selectedNodeId: null,
        selectedEdgeId: null,
        pendingConnection: '',
        pendingTemplateType: '',
        dragState: null,
        connectionDrag: null,
        suppressNextInputClick: false,
        canvasDropPreview: null,
        canvasZoom: 1,
        statusMessage: 'Add a step from the library, then connect the flow from left to right.',
        yamlPanel: { open: false, mode: 'import', text: '' },
        runForm: { agent_name: '', input: '' },
        runState: { status: '', message: '' },
        currentRunStream: null,

        init() {
            this.graph = this.parseJsonScript('pipeline-builder-graph');
            this.agentNames = this.parseJsonScript('pipeline-builder-agents');
            this.toolNames = this.parseJsonScript('pipeline-builder-tools');
            this.normalizeGraph();
            this.autoLayoutIfNeeded();

            var self = this;
            window.addEventListener('mousemove', function (event) {
                self.handleGlobalPointerMove(event);
            });
            window.addEventListener('mouseup', function () {
                self.finishGlobalPointer();
            });
            window.addEventListener('keydown', function (event) {
                self.handleKeydown(event);
            });
        },

        parseJsonScript(id) {
            var el = document.getElementById(id);
            if (!el) return [];
            try {
                return JSON.parse(el.textContent);
            } catch (_) {
                return [];
            }
        },

        normalizeGraph() {
            if (!this.graph || typeof this.graph !== 'object') {
                this.graph = {};
            }
            this.graph.name = this.graph.name || '';
            this.graph.version = this.graph.version || '1.0.0';
            this.graph.description = this.graph.description || '';
            this.graph.output = this.graph.output || null;
            this.graph.max_cost_usd = this.graph.max_cost_usd ?? null;
            this.graph.max_wall_time_minutes = this.graph.max_wall_time_minutes ?? null;
            this.graph.nodes = Array.isArray(this.graph.nodes) ? this.graph.nodes : [];
            this.graph.edges = Array.isArray(this.graph.edges) ? this.graph.edges : [];

            this.graph.nodes = this.graph.nodes.map(function (node) {
                node.config = node.config || {};
                node.runStatus = node.runStatus || '';
                if (typeof node.x !== 'number') node.x = 0;
                if (typeof node.y !== 'number') node.y = 0;
                return node;
            });

            this.ensureStructuralNodes();
            this.graph.edges = this.graph.edges.filter(function (edge, index, edges) {
                if (!edge || !edge.source || !edge.target) return false;
                return edges.findIndex(function (candidate) {
                    return candidate.source === edge.source && candidate.target === edge.target;
                }) === index;
            });
        },

        ensureStructuralNodes() {
            if (!this.findNode('__start__')) {
                this.graph.nodes.unshift(this.makeNode('start', 40, 260));
            }
            if (!this.findNode('__end__')) {
                this.graph.nodes.push(this.makeNode('end', 980, 260));
            }
        },

        autoLayoutIfNeeded() {
            var executable = this.graph.nodes.filter(function (node) {
                return node.node_type !== 'start' && node.node_type !== 'end';
            });
            if (!executable.length) {
                var startNode = this.findNode('__start__');
                var endNode = this.findNode('__end__');
                if (startNode) {
                    startNode.x = 40;
                    startNode.y = 260;
                }
                if (endNode) {
                    endNode.x = 980;
                    endNode.y = 260;
                }
                return;
            }

            var needsLayout = executable.every(function (node) {
                return node.x === 0 && node.y === 0;
            });
            if (needsLayout) {
                this.autoLayout();
            }
        },

        makeNode(type, x, y) {
            if (type === 'start') {
                return { id: '__start__', node_type: 'start', label: 'Start', x: x, y: y, config: {}, runStatus: '' };
            }
            if (type === 'end') {
                return { id: '__end__', node_type: 'end', label: 'End', x: x, y: y, config: {}, runStatus: '' };
            }
            var id = type + '-' + Math.random().toString(36).slice(2, 9);
            return {
                id: id,
                node_type: type,
                label: type === 'agent' ? 'Agent Step' : 'Tool Step',
                x: x,
                y: y,
                config: {
                    agent_name: '',
                    task: '',
                    tool_name: '',
                    tool_input_json: type === 'tool' ? '{}' : '',
                    output_var: '',
                    timeout_minutes: null,
                    retry_on_failure: null,
                    retry_backoff_ms: null,
                    retry_max_delay_ms: null,
                    on_failure: 'fail',
                    default_value: ''
                },
                runStatus: ''
            };
        },

        templateLabel(type) {
            if (type === 'agent') return 'Agent';
            if (type === 'tool') return 'Tool';
            return 'Step';
        },

        nodeTypeLabel(type) {
            if (type === 'start') return 'Trigger';
            if (type === 'end') return 'Output';
            return this.templateLabel(type);
        },

        inspectorSubtitle(node) {
            if (!node) return '';
            if (node.node_type === 'agent') {
                return node.config.agent_name ? 'Runs ' + node.config.agent_name : 'Configure which agent should run this step';
            }
            if (node.node_type === 'tool') {
                return node.config.tool_name ? 'Calls ' + node.config.tool_name : 'Configure which tool should run here';
            }
            if (node.node_type === 'start') return 'Entry point for workflow input';
            if (node.node_type === 'end') return 'Final output of the workflow';
            return '';
        },

        startTemplateDrag(event, type) {
            this.pendingTemplateType = '';
            event.dataTransfer.setData('nodeType', type);
            event.dataTransfer.effectAllowed = 'copy';
        },

        armTemplatePlacement(type) {
            this.pendingTemplateType = type;
            this.statusMessage = 'Click anywhere on the canvas to place a new ' + this.templateLabel(type).toLowerCase() + ' step.';
        },

        clearTemplatePlacement() {
            this.pendingTemplateType = '';
        },

        updateDropPreview(event) {
            var type = event.dataTransfer.getData('nodeType');
            if (!type) return;
            this.canvasDropPreview = this.canvasPoint(event.clientX, event.clientY);
        },

        clearDropPreview() {
            this.canvasDropPreview = null;
        },

        canvasDropPreviewStyle() {
            if (!this.canvasDropPreview) return '';
            return 'left:' + (this.canvasDropPreview.x - 100) + 'px; top:' + (this.canvasDropPreview.y - 48) + 'px;';
        },

        handleCanvasClick(event) {
            if (this.pendingTemplateType) {
                var point = this.canvasPoint(event.clientX, event.clientY);
                this.insertNode(this.pendingTemplateType, point.x - 110, point.y - 52, { autoConnect: true });
                this.clearTemplatePlacement();
                return;
            }
            this.clearSelection();
        },

        dropTemplate(event) {
            var type = event.dataTransfer.getData('nodeType');
            this.clearDropPreview();
            if (!type) return;
            var point = this.canvasPoint(event.clientX, event.clientY);
            this.insertNode(type, point.x - 110, point.y - 52, { autoConnect: true });
        },

        addNodeFromPalette(type) {
            var x = 260;
            var y = 200;
            var anchor = this.preferredInsertionSource();
            if (anchor) {
                x = anchor.x + 260;
                y = anchor.y;
            }
            this.insertNode(type, x, y, { autoConnect: true, anchorId: anchor ? anchor.id : '' });
        },

        insertStarterFlow() {
            var existing = this.executableNodeCount();
            if (existing > 0) {
                this.statusMessage = 'Starter flow is meant for an empty canvas. Use Add Agent or Add Tool to grow the current workflow.';
                return;
            }
            var agent = this.insertNode('agent', 280, 200, { autoConnect: false, select: false });
            var tool = this.insertNode('tool', 580, 200, { autoConnect: false, select: false });
            this.connectNodes('__start__', agent.id);
            this.connectNodes(agent.id, tool.id);
            this.connectNodes(tool.id, '__end__');
            this.selectNode(agent.id);
            this.statusMessage = 'Starter flow added. Configure the agent prompt and tool input to make it your own.';
        },

        insertNode(type, x, y, options) {
            options = options || {};
            var node = this.makeNode(type, Math.max(40, Math.round(x)), Math.max(40, Math.round(y)));
            this.graph.nodes.push(node);
            if (options.autoConnect) {
                this.autoConnectNewNode(node, options.anchorId || '');
            }
            if (options.select !== false) {
                this.selectNode(node.id);
            }
            this.statusMessage = 'Added a new ' + this.templateLabel(type).toLowerCase() + ' step.';
            return node;
        },

        preferredInsertionSource() {
            var selected = this.selectedNode();
            if (selected && selected.node_type !== 'end') {
                return selected;
            }

            var endIncoming = this.graph.edges.filter(function (edge) {
                return edge.target === '__end__';
            });
            if (endIncoming.length) {
                return this.findNode(endIncoming[endIncoming.length - 1].source);
            }

            var executable = this.graph.nodes.filter(function (node) {
                return node.node_type !== 'start' && node.node_type !== 'end';
            }).sort(function (a, b) {
                if (a.x !== b.x) return a.x - b.x;
                return a.y - b.y;
            });
            return executable.length ? executable[executable.length - 1] : this.findNode('__start__');
        },

        preferredInsertionSourceId(excludeNodeId) {
            var selected = this.selectedNode();
            if (selected && selected.id !== excludeNodeId && selected.node_type !== 'end') {
                return selected.id;
            }

            var endIncoming = this.graph.edges.filter(function (edge) {
                return edge.target === '__end__' && edge.source !== excludeNodeId;
            });
            if (endIncoming.length) {
                return endIncoming[endIncoming.length - 1].source;
            }

            var executable = this.graph.nodes.filter(function (node) {
                return node.node_type !== 'start' && node.node_type !== 'end' && node.id !== excludeNodeId;
            }).sort(function (a, b) {
                if (a.x !== b.x) return a.x - b.x;
                return a.y - b.y;
            });

            return executable.length ? executable[executable.length - 1].id : '__start__';
        },

        autoConnectNewNode(node, anchorId) {
            if (!node || node.node_type === 'start' || node.node_type === 'end') return;
            var sourceId = anchorId || this.preferredInsertionSourceId(node.id);
            if (!sourceId || sourceId === '__end__') {
                sourceId = '__start__';
            }

            var previousEndEdge = this.graph.edges.find(function (edge) {
                return edge.source === sourceId && edge.target === '__end__';
            });

            if (sourceId !== node.id) {
                this.connectNodes(sourceId, node.id);
            }

            if (previousEndEdge) {
                this.deleteEdge(previousEndEdge.id, true);
                this.connectNodes(node.id, '__end__');
                return;
            }

            if (!this.graph.edges.some(function (edge) { return edge.target === '__end__'; })) {
                this.connectNodes(node.id, '__end__');
            }
        },

        selectedNode() {
            return this.findNode(this.selectedNodeId);
        },

        selectedEdge() {
            return this.findEdge(this.selectedEdgeId);
        },

        findNode(id) {
            return this.graph.nodes.find(function (node) { return node.id === id; });
        },

        findEdge(id) {
            return this.graph.edges.find(function (edge) { return edge.id === id; });
        },

        selectNode(id) {
            this.selectedNodeId = id;
            this.selectedEdgeId = null;
        },

        selectEdge(id) {
            this.selectedEdgeId = id;
            this.selectedNodeId = null;
        },

        clearSelection() {
            this.selectedNodeId = null;
            this.selectedEdgeId = null;
            this.pendingConnection = '';
        },

        canDeleteSelected() {
            var node = this.selectedNode();
            return !!node && node.id !== '__start__' && node.id !== '__end__';
        },

        deleteSelectedNode() {
            var node = this.selectedNode();
            if (!node || !this.canDeleteSelected()) return;
            this.graph.nodes = this.graph.nodes.filter(function (candidate) { return candidate.id !== node.id; });
            this.graph.edges = this.graph.edges.filter(function (edge) {
                return edge.source !== node.id && edge.target !== node.id;
            });
            this.selectedNodeId = null;
            this.statusMessage = 'Removed ' + node.label + ' and its connections.';
        },

        deleteSelectedEdge() {
            if (!this.selectedEdgeId) return;
            this.deleteEdge(this.selectedEdgeId);
        },

        deleteEdge(id, silent) {
            var edge = this.findEdge(id);
            if (!edge) return;
            this.graph.edges = this.graph.edges.filter(function (candidate) { return candidate.id !== id; });
            if (this.selectedEdgeId === id) {
                this.selectedEdgeId = null;
            }
            if (!silent) {
                this.statusMessage = 'Removed the connection from ' + this.edgeLabel(edge.source) + ' to ' + this.edgeLabel(edge.target) + '.';
            }
        },

        incomingEdgesForSelected() {
            if (!this.selectedNodeId) return [];
            var nodeId = this.selectedNodeId;
            return this.graph.edges.filter(function (edge) { return edge.target === nodeId; });
        },

        outgoingEdgesForSelected() {
            if (!this.selectedNodeId) return [];
            var nodeId = this.selectedNodeId;
            return this.graph.edges.filter(function (edge) { return edge.source === nodeId; });
        },

        edgeLabel(nodeId) {
            var node = this.findNode(nodeId);
            return node ? node.label : nodeId;
        },

        beginNodeDrag(event, node) {
            if (event.target.classList.contains('pipeline-port')) return;
            var canvasPoint = this.canvasPoint(event.clientX, event.clientY);
            this.dragState = {
                nodeId: node.id,
                offsetX: canvasPoint.x - node.x,
                offsetY: canvasPoint.y - node.y
            };
            this.selectNode(node.id);
        },

        beginConnectionDrag(event, node) {
            if (!node || node.node_type === 'end') return;
            var point = this.canvasPoint(event.clientX, event.clientY);
            this.connectionDrag = {
                sourceId: node.id,
                x: point.x,
                y: point.y,
                completed: false
            };
            this.pendingConnection = node.id;
            this.selectedEdgeId = null;
            this.statusMessage = 'Drag to another step input, or click an input port to finish the connection.';
        },

        finishConnectionDrag(node) {
            var sourceId = this.connectionDrag ? this.connectionDrag.sourceId : this.pendingConnection;
            if (!sourceId) return;
            this.tryCreateConnection(sourceId, node.id);
            if (this.connectionDrag) {
                this.connectionDrag.completed = true;
            }
            this.suppressNextInputClick = true;
            this.connectionDrag = null;
            this.pendingConnection = '';
        },

        handleGlobalPointerMove(event) {
            if (this.dragState) {
                var dragged = this.findNode(this.dragState.nodeId);
                if (!dragged) return;
                var point = this.canvasPoint(event.clientX, event.clientY);
                dragged.x = Math.max(20, Math.round(point.x - this.dragState.offsetX));
                dragged.y = Math.max(20, Math.round(point.y - this.dragState.offsetY));
            }
            if (this.connectionDrag) {
                var edgePoint = this.canvasPoint(event.clientX, event.clientY);
                this.connectionDrag.x = edgePoint.x;
                this.connectionDrag.y = edgePoint.y;
            }
        },

        finishGlobalPointer() {
            this.dragState = null;
            if (this.connectionDrag && !this.connectionDrag.completed) {
                this.statusMessage = 'Select a target input to finish the connection.';
            }
            this.connectionDrag = null;
        },

        handlePortClick(node, direction) {
            if (direction === 'input' && this.suppressNextInputClick) {
                this.suppressNextInputClick = false;
                return;
            }
            if (direction === 'output') {
                if (node.node_type === 'end') return;
                this.pendingConnection = node.id;
                this.statusMessage = 'Choose a target step to finish the connection.';
                return;
            }

            if (!this.pendingConnection) {
                this.statusMessage = 'Choose an output port first.';
                return;
            }
            this.tryCreateConnection(this.pendingConnection, node.id);
            this.pendingConnection = '';
        },

        tryCreateConnection(sourceId, targetId) {
            if (targetId === '__start__') {
                this.statusMessage = 'The Start step cannot receive incoming connections.';
                return;
            }
            if (sourceId === '__end__') {
                this.statusMessage = 'The End step cannot start a connection.';
                return;
            }
            if (sourceId === targetId) {
                this.statusMessage = 'A step cannot connect to itself.';
                return;
            }
            var exists = this.graph.edges.some(function (edge) {
                return edge.source === sourceId && edge.target === targetId;
            });
            if (exists) {
                this.statusMessage = 'Those steps are already connected.';
                return;
            }
            if (targetId === '__end__') {
                this.graph.edges = this.graph.edges.filter(function (edge) {
                    return edge.target !== '__end__';
                });
            }
            this.connectNodes(sourceId, targetId);
            this.statusMessage = 'Connected ' + this.edgeLabel(sourceId) + ' to ' + this.edgeLabel(targetId) + '.';
        },

        connectNodes(sourceId, targetId) {
            var edgeId = 'edge-' + sourceId + '-' + targetId;
            var suffix = 1;
            while (this.findEdge(edgeId)) {
                suffix += 1;
                edgeId = 'edge-' + sourceId + '-' + targetId + '-' + suffix;
            }
            this.graph.edges.push({ id: edgeId, source: sourceId, target: targetId });
            return edgeId;
        },

        nodeClasses(node) {
            return {
                'pipeline-node-start': node.node_type === 'start',
                'pipeline-node-agent': node.node_type === 'agent',
                'pipeline-node-tool': node.node_type === 'tool',
                'pipeline-node-end': node.node_type === 'end',
                'pipeline-node-selected': this.selectedNodeId === node.id,
                'pipeline-node-running': node.runStatus === 'running',
                'pipeline-node-complete': node.runStatus === 'complete',
                'pipeline-node-failed': node.runStatus === 'failed',
                'pipeline-node-skipped': node.runStatus === 'skipped'
            };
        },

        edgeClasses(edge) {
            return {
                'pipeline-edge-active': edge.source === this.pendingConnection,
                'pipeline-edge-selected': this.selectedEdgeId === edge.id
            };
        },

        nodeStyle(node) {
            return 'left:' + node.x + 'px; top:' + node.y + 'px;';
        },

        edgePath(edge) {
            var source = this.findNode(edge.source);
            var target = this.findNode(edge.target);
            if (!source || !target) return '';
            var sourcePort = this.nodeOutputPort(source);
            var targetPort = this.nodeInputPort(target);
            return this.bezierPath(sourcePort.x, sourcePort.y, targetPort.x, targetPort.y);
        },

        connectionPreviewPath() {
            if (!this.connectionDrag) return '';
            var source = this.findNode(this.connectionDrag.sourceId);
            if (!source) return '';
            var sourcePort = this.nodeOutputPort(source);
            return this.bezierPath(sourcePort.x, sourcePort.y, this.connectionDrag.x, this.connectionDrag.y);
        },

        bezierPath(sx, sy, tx, ty) {
            var delta = Math.max(80, Math.abs(tx - sx) * 0.45);
            return 'M ' + sx + ' ' + sy + ' C ' + (sx + delta) + ' ' + sy + ', ' + (tx - delta) + ' ' + ty + ', ' + tx + ' ' + ty;
        },

        nodeInputPort(node) {
            return { x: node.x, y: node.y + 58 };
        },

        nodeOutputPort(node) {
            return { x: node.x + 220, y: node.y + 58 };
        },

        canvasPoint(clientX, clientY) {
            var rect = this.$refs.canvas.getBoundingClientRect();
            return {
                x: Math.round((clientX - rect.left + this.$refs.canvas.scrollLeft) / this.canvasZoom),
                y: Math.round((clientY - rect.top + this.$refs.canvas.scrollTop) / this.canvasZoom)
            };
        },

        canvasStageStyle() {
            var bounds = this.graphBounds();
            return 'zoom:' + this.canvasZoom + '; width:' + bounds.width + 'px; height:' + bounds.height + 'px;';
        },

        graphBounds() {
            var maxX = 1200;
            var maxY = 760;
            this.graph.nodes.forEach(function (node) {
                maxX = Math.max(maxX, node.x + 320);
                maxY = Math.max(maxY, node.y + 220);
            });
            return { width: maxX, height: maxY };
        },

        zoomIn() {
            this.canvasZoom = Math.min(1.6, Math.round((this.canvasZoom + 0.1) * 10) / 10);
        },

        zoomOut() {
            this.canvasZoom = Math.max(0.7, Math.round((this.canvasZoom - 0.1) * 10) / 10);
        },

        resetCanvasView() {
            this.canvasZoom = 1;
            if (this.$refs.canvas) {
                this.$refs.canvas.scrollLeft = 0;
                this.$refs.canvas.scrollTop = 0;
            }
        },

        autoLayout() {
            var executable = this.graph.nodes.filter(function (node) {
                return node.node_type !== 'start' && node.node_type !== 'end';
            });
            var incoming = {};
            executable.forEach(function (node) {
                incoming[node.id] = [];
            });
            this.graph.edges.forEach(function (edge) {
                if (incoming[edge.target]) {
                    incoming[edge.target].push(edge.source);
                }
            });

            var depthCache = {};
            var self = this;
            function depth(nodeId, stack) {
                if (depthCache[nodeId]) return depthCache[nodeId];
                if (stack.indexOf(nodeId) !== -1) return 1;
                var deps = incoming[nodeId] || [];
                var executableDeps = deps.filter(function (dep) {
                    return dep !== '__start__' && dep !== '__end__';
                });
                if (!executableDeps.length) {
                    depthCache[nodeId] = 1;
                    return 1;
                }
                var result = 1 + Math.max.apply(null, executableDeps.map(function (dep) {
                    return depth(dep, stack.concat(nodeId));
                }));
                depthCache[nodeId] = result;
                return result;
            }

            executable.forEach(function (node) {
                depth(node.id, []);
            });

            var lanes = {};
            executable.sort(function (a, b) {
                var depthA = depthCache[a.id] || 1;
                var depthB = depthCache[b.id] || 1;
                if (depthA !== depthB) return depthA - depthB;
                return a.id.localeCompare(b.id);
            }).forEach(function (node) {
                var lane = lanes[depthCache[node.id]] || 0;
                node.x = 260 + ((depthCache[node.id] - 1) * 280);
                node.y = 120 + (lane * 160);
                lanes[depthCache[node.id]] = lane + 1;
            });

            var startNode = this.findNode('__start__');
            var endNode = this.findNode('__end__');
            if (startNode) {
                startNode.x = 40;
                startNode.y = 260;
            }
            if (endNode) {
                var maxDepth = 1;
                Object.keys(depthCache).forEach(function (key) {
                    maxDepth = Math.max(maxDepth, depthCache[key]);
                });
                endNode.x = 260 + (maxDepth * 280);
                endNode.y = 260;
            }

            this.statusMessage = 'Auto-layout arranged the workflow from left to right.';
        },

        executableNodeCount() {
            return this.graph.nodes.filter(function (node) {
                return node.node_type !== 'start' && node.node_type !== 'end';
            }).length;
        },

        clearRunState() {
            this.graph.nodes.forEach(function (node) { node.runStatus = ''; });
            this.runState = { status: '', message: '' };
            this.statusMessage = 'Run state cleared from the canvas.';
        },

        handleKeydown(event) {
            if (event.key !== 'Delete' && event.key !== 'Backspace') return;
            if (event.target && /INPUT|TEXTAREA|SELECT/.test(event.target.tagName)) return;
            if (this.selectedEdgeId) {
                event.preventDefault();
                this.deleteSelectedEdge();
                return;
            }
            if (this.selectedNodeId && this.canDeleteSelected()) {
                event.preventDefault();
                this.deleteSelectedNode();
            }
        },

        csrfToken() {
            var meta = document.querySelector('meta[name="csrf-token"]');
            return meta ? meta.content : '';
        },

        graphPayload() {
            var normalizedNodes = this.graph.nodes.map(function (node) {
                return {
                    id: node.id,
                    node_type: node.node_type,
                    label: node.label,
                    x: Math.round(node.x),
                    y: Math.round(node.y),
                    config: {
                        agent_name: node.config.agent_name || '',
                        task: node.config.task || '',
                        tool_name: node.config.tool_name || '',
                        tool_input_json: node.config.tool_input_json || '',
                        output_var: node.config.output_var || '',
                        timeout_minutes: this.normalizeNullableNumber(node.config.timeout_minutes),
                        retry_on_failure: this.normalizeNullableNumber(node.config.retry_on_failure),
                        retry_backoff_ms: this.normalizeNullableNumber(node.config.retry_backoff_ms),
                        retry_max_delay_ms: this.normalizeNullableNumber(node.config.retry_max_delay_ms),
                        on_failure: node.config.on_failure || 'fail',
                        default_value: node.config.default_value || ''
                    }
                };
            }, this);
            return {
                graph: {
                    name: this.graph.name,
                    version: this.graph.version,
                    description: this.graph.description,
                    output: this.graph.output || null,
                    max_cost_usd: this.normalizeNullableNumber(this.graph.max_cost_usd),
                    max_wall_time_minutes: this.normalizeNullableNumber(this.graph.max_wall_time_minutes),
                    nodes: normalizedNodes,
                    edges: this.graph.edges
                }
            };
        },

        normalizeNullableNumber(value) {
            if (value === '' || value === null || typeof value === 'undefined') return null;
            var parsed = Number(value);
            return Number.isFinite(parsed) ? parsed : null;
        },

        async savePipeline() {
            this.statusMessage = 'Saving pipeline...';
            var response = await fetch('/api/pipelines', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'X-CSRF-Token': this.csrfToken()
                },
                body: JSON.stringify(this.graphPayload())
            });
            if (!response.ok) {
                this.statusMessage = await response.text();
                window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: this.statusMessage, type: 'error' } }));
                return;
            }
            var payload = await response.json();
            this.statusMessage = 'Saved ' + payload.name + ' (' + payload.step_count + ' steps).';
            window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: 'Pipeline saved successfully', type: 'success' } }));
        },

        openYamlPanel(mode) {
            this.yamlPanel.mode = mode;
            this.yamlPanel.text = '';
            this.yamlPanel.open = true;
        },

        closeYamlPanel() {
            this.yamlPanel.open = false;
        },

        async importYaml() {
            var response = await fetch('/api/pipelines/import', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'X-CSRF-Token': this.csrfToken()
                },
                body: JSON.stringify({ yaml: this.yamlPanel.text })
            });
            if (!response.ok) {
                this.statusMessage = await response.text();
                window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: this.statusMessage, type: 'error' } }));
                return;
            }
            this.graph = await response.json();
            this.normalizeGraph();
            this.autoLayoutIfNeeded();
            this.closeYamlPanel();
            this.statusMessage = 'Imported pipeline YAML into the canvas.';
        },

        async exportYaml() {
            var response = await fetch('/api/pipelines/export', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'X-CSRF-Token': this.csrfToken()
                },
                body: JSON.stringify(this.graphPayload())
            });
            if (!response.ok) {
                this.statusMessage = await response.text();
                window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: this.statusMessage, type: 'error' } }));
                return;
            }
            var payload = await response.json();
            this.yamlPanel.mode = 'export';
            this.yamlPanel.text = payload.yaml;
            this.yamlPanel.open = true;
            this.statusMessage = 'Exported the current pipeline YAML.';
        },

        downloadYaml() {
            var blob = new Blob([this.yamlPanel.text], { type: 'application/yaml;charset=utf-8' });
            var url = URL.createObjectURL(blob);
            var link = document.createElement('a');
            link.href = url;
            link.download = (this.graph.name || 'pipeline') + '.yaml';
            link.click();
            URL.revokeObjectURL(url);
        },

        async runPipeline() {
            if (!this.runForm.agent_name) {
                this.statusMessage = 'Agent name is required to run a pipeline.';
                return;
            }
            await this.savePipeline();
            if (this.statusMessage.indexOf('Saved ') !== 0) return;
            this.graph.nodes.forEach(function (node) { node.runStatus = ''; });
            var response = await fetch('/api/pipelines/run', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'X-CSRF-Token': this.csrfToken()
                },
                body: JSON.stringify({
                    pipeline_name: this.graph.name,
                    input: this.runForm.input || '',
                    agent_name: this.runForm.agent_name
                })
            });
            if (!response.ok) {
                this.runState = { status: 'failed', message: await response.text() };
                return;
            }
            var payload = await response.json();
            this.runState = { status: payload.status, message: 'Streaming live step updates...' };
            this.connectRunStream(payload.run_id);
        },

        connectRunStream(runId) {
            if (this.currentRunStream) {
                this.currentRunStream.close();
            }
            var self = this;
            this.currentRunStream = new EventSource('/api/pipelines/runs/' + encodeURIComponent(runId) + '/events');
            this.currentRunStream.addEventListener('step-status', function (event) {
                try {
                    var payload = JSON.parse(event.data);
                    var node = self.findNode(payload.step_id);
                    if (node) {
                        node.runStatus = payload.status;
                    }
                } catch (_) {}
            });
            this.currentRunStream.addEventListener('run-status', function (event) {
                try {
                    var payload = JSON.parse(event.data);
                    self.runState = {
                        status: payload.status,
                        message: payload.error || payload.output || 'Pipeline run updated.'
                    };
                    if (payload.status !== 'running' && self.currentRunStream) {
                        self.currentRunStream.close();
                        self.currentRunStream = null;
                    }
                } catch (_) {}
            });
            this.currentRunStream.addEventListener('pipeline-error', function (event) {
                self.runState = { status: 'failed', message: event.data || 'Pipeline event stream failed.' };
                if (self.currentRunStream) {
                    self.currentRunStream.close();
                    self.currentRunStream = null;
                }
            });
        }
    };
}
