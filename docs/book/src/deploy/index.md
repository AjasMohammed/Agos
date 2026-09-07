# Deployment

AgentOS ships hardened deployment artifacts for every common environment. All deploy modes
expose the same observability surfaces (`/healthz`, `/readyz`, `/metrics` on port `9091`)
and use the same configuration model.

Pick a mode:

- **[systemd (host)](./systemd.md)** — run the kernel as a hardened system service on a
  Linux host, with watchdog, resource limits, and read-only filesystem.
- **[Docker & Compose](./docker.md)** — the multi-stage image plus a Compose stack with
  Ollama and Jaeger, read-only rootfs, and persistent volumes.
- **[Kubernetes (Helm)](./kubernetes.md)** — the `deploy/helm/agentos` chart with a PVC,
  Secret-backed vault passphrase, non-root pod security context, and optional ingress.
- **[Gateway-first (run as a bot)](./gateway.md)** — `agentos gateway run`: boot the kernel
  and connect messaging channels (Telegram, Discord, Slack, …) as a long-lived bot.
- **[Observability](./observability.md)** — Prometheus metrics, JSON logs, Grafana, and
  OpenTelemetry traces to Jaeger.

Before any production deployment, review the **[Configuration Reference](../configuration.md)**
(persistent paths, vault passphrase sourcing) and the **[Security](../security.md)** page
(trust boundaries, the seccomp-Linux caveat, signed releases).
