# Kubernetes (Helm)

The chart at `deploy/helm/agentos` deploys the kernel with a persistent volume, a
Secret-backed vault passphrase, a non-root pod security context, and optional ingress.

## Install

```bash
helm install agentos deploy/helm/agentos \
  --set vault.passphrase='<your-secret>' \
  --set config.ollamaHost=http://ollama:11434
```

Or reference an existing Secret instead of inlining the passphrase:

```bash
helm install agentos deploy/helm/agentos \
  --set vault.existingSecret=agentos-vault
```

## What the chart provisions

- **Deployment** running the `ghcr.io/ajasmohammed/agos` image, with container ports `8080` (web/API)
  and `9091` (health/metrics).
- **PersistentVolumeClaim** — `persistence.enabled: true`, `10Gi`, `ReadWriteOnce` by
  default (set `persistence.storageClass` for your cluster).
- **Secret** — holds the vault passphrase; mounted as `AGENTOS_VAULT_PASSPHRASE`.
- **Service** (`ClusterIP`) exposing the web port and the health port.
- **ServiceAccount**, **ConfigMap** (kernel config/env), and an optional **Ingress**.

## Hardened pod security context

```yaml
securityContext:
  allowPrivilegeEscalation: false
  readOnlyRootFilesystem: true
  runAsNonRoot: true
  runAsUser: 65532
  runAsGroup: 65532
podSecurityContext:
  fsGroup: 65532
```

Default resource envelope: requests `250m` CPU / `512Mi` memory, limits `2000m` CPU /
`2Gi` memory.

## Key values

| Value | Default | Purpose |
|-------|---------|---------|
| `replicaCount` | `1` | Pod replicas (single-writer state DB — keep at 1 unless you separate volumes). |
| `vault.passphrase` / `vault.existingSecret` | `""` | Vault passphrase source. |
| `persistence.size` | `10Gi` | Data volume size. |
| `config.autoInitVault` | `true` | Auto-init the vault on first boot. |
| `config.ollamaHost` | `http://ollama:11434` | Inference backend. |
| `config.otelEnabled` / `config.otelEndpoint` | `false` / `""` | OpenTelemetry export. |
| `ingress.enabled` | `false` | Expose the web/API port via Ingress. |

## Health & probes

The kernel serves `/healthz`, `/readyz`, and `/metrics` on the health port (`9091`). Wire
these to Kubernetes liveness/readiness probes and a Prometheus `ServiceMonitor` (see
[Observability](./observability.md)).
