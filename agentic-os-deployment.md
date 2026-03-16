# AgentOS — Deployment Plans & Architecture

> *From Docker container to bare metal — a staged deployment roadmap for AgentOS.*

---

## Table of Contents

1. [Deployment Philosophy](#deployment-philosophy)
2. [The Three Deployment Stages](#the-three-deployment-stages)
3. [Stage 1 — Docker on Linux](#stage-1--docker-on-linux)
4. [Stage 2 — Minimal Linux Base Image](#stage-2--minimal-linux-base-image)
5. [Stage 3 — Bare Metal Rust Kernel](#stage-3--bare-metal-rust-kernel)
6. [Host Requirements](#host-requirements)
7. [Cloud Deployment](#cloud-deployment)
8. [Self-Hosted / On-Premise Deployment](#self-hosted--on-premise-deployment)
9. [Edge & Embedded Deployment](#edge--embedded-deployment)
10. [Networking Architecture](#networking-architecture)
11. [Storage Architecture](#storage-architecture)
12. [GPU Deployment](#gpu-deployment)
13. [High Availability & Scaling](#high-availability--scaling)
14. [Security Hardening for Deployment](#security-hardening-for-deployment)
15. [Upgrade & Migration Strategy](#upgrade--migration-strategy)
16. [Monitoring & Observability](#monitoring--observability)
17. [Security Gate — Required before launch](#security-gate--required-before-launch)
18. [Deployment Checklist](#deployment-checklist)

---

## Deployment Philosophy

AgentOS is designed around one core deployment principle: **the host environment should be invisible.**

Just as a person sitting at a Linux workstation doesn't think about the BIOS or the bootloader, the agents running inside AgentOS should never be aware of — or affected by — whatever sits beneath it. The host OS is plumbing. AgentOS is the environment agents actually live in.

This shapes every deployment decision:

- The host layer should be as **thin as possible**
- Every external dependency should be **explicitly declared and versioned**
- Deployment should be **repeatable** — same behavior on a $5 VPS and a $50,000 server
- **Data** (vault, semantic store, episodic memory) must **survive container restarts**
- **Security** posture must be enforced at the deployment level, not assumed from the host

---

## The Three Deployment Stages

AgentOS deployment evolves in three distinct stages, each progressively thinner on the host side:

```
Stage 1: Docker on Linux
┌────────────────────────────────────┐
│          AgentOS Container         │  ← What agents see
├────────────────────────────────────┤
│         Docker / containerd        │  ← Container runtime
├────────────────────────────────────┤
│      Linux Kernel (any distro)     │  ← Host OS (visible to operator)
├────────────────────────────────────┤
│      Hardware / Hypervisor         │
└────────────────────────────────────┘

Stage 2: Minimal Linux Base
┌────────────────────────────────────┐
│          AgentOS Container         │  ← What agents see
├────────────────────────────────────┤
│         Docker / containerd        │
├────────────────────────────────────┤
│   Minimal Linux (Alpine / Custom)  │  ← Thin, invisible to agents
├────────────────────────────────────┤
│      Hardware / Hypervisor         │
└────────────────────────────────────┘

Stage 3: Bare Metal Rust Kernel
┌────────────────────────────────────┐
│             AgentOS                │  ← Agents + OS are the same thing
├────────────────────────────────────┤
│     AgentOS Rust Bootloader        │  ← Boots directly on hardware
├────────────────────────────────────┤
│          Hardware / UEFI           │  ← No Linux, no hypervisor
└────────────────────────────────────┘
```

---

## Stage 1 — Docker on Linux

This is the **production-ready shipping target** for the first version of AgentOS. It is not a compromise — the vast majority of modern infrastructure (Kubernetes, cloud services, CI/CD systems) runs exactly this way.

### Why Docker First

- Zero dependency on a specific Linux distribution — works on Ubuntu, Debian, Alpine, RHEL, anything
- Operators can run AgentOS on any server, VPS, or cloud VM they already have
- Container boundaries provide an additional isolation layer on top of AgentOS's internal security
- Rollback is trivial — pull the previous image tag
- The agent never knows or cares about the Linux host beneath

### Directory Structure

```
/opt/agentos/                    # AgentOS root (bind-mounted into container)
├── vault/                       # Encrypted secrets vault (SQLCipher)
│   └── vault.db                 # AES-256-GCM encrypted credential store
├── data/                        # Semantic store + episodic memory
│   ├── semantic/                # Vector store (embeddings)
│   └── episodic/                # Per-task SQLite databases
├── tools/                       # Installed tools
│   ├── core/                    # Core tools (shipped with AgentOS)
│   └── user/                    # User-installed tools from registry
├── agents/                      # Agent profiles and registry
│   └── registry.db              # Agent identity + permission matrix
├── logs/                        # Append-only audit log
│   └── audit.log
├── config/                      # System configuration
│   └── agentos.toml
└── pipelines/                   # User-defined multi-agent pipelines
    └── *.yaml
```

### Dockerfile

```dockerfile
# ─── Build Stage ───────────────────────────────────────────────
FROM rust:1.78-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev openssl-dev pkgconf

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY tools/ ./tools/

# Build the kernel binary (statically linked for portability)
RUN cargo build --release --bin agentos-kernel --target x86_64-unknown-linux-musl

# Build core tools
RUN cargo build --release --bin agentos-tools --target x86_64-unknown-linux-musl

# ─── Runtime Stage ─────────────────────────────────────────────
FROM alpine:3.19

# Runtime dependencies only
RUN apk add --no-cache \
    wasmtime \           # WASM tool sandbox runtime
    sqlcipher \          # Encrypted vault storage
    libgcc \             # Rust runtime
    ca-certificates      # TLS for LLM API connections

# Copy binaries from build stage
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/agentos-kernel /usr/bin/
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/agentos-tools /usr/bin/
COPY --from=builder /build/tools/core/ /opt/agentos/tools/core/

# Copy default configuration
COPY config/default.toml /opt/agentos/config/agentos.toml

# Create directory structure
RUN mkdir -p \
    /opt/agentos/vault \
    /opt/agentos/data/semantic \
    /opt/agentos/data/episodic \
    /opt/agentos/tools/user \
    /opt/agentos/agents \
    /opt/agentos/logs \
    /opt/agentos/pipelines

# Persistent volumes — these must survive container restarts
VOLUME ["/opt/agentos/vault", "/opt/agentos/data", "/opt/agentos/agents", "/opt/agentos/logs"]

# Web UI
EXPOSE 8080
# Internal kernel API (should NOT be exposed to host network)
# EXPOSE 9090   ← intentionally not exposed

# Non-root user for security
RUN addgroup -S agentos && adduser -S agentos -G agentos
RUN chown -R agentos:agentos /opt/agentos
USER agentos

ENTRYPOINT ["/usr/bin/agentos-kernel"]
CMD ["--config", "/opt/agentos/config/agentos.toml"]
```

### docker-compose.yml (Standard Deployment)

```yaml
version: "3.9"

services:

  # ── AgentOS Kernel ─────────────────────────────────────────────
  agentos:
    image: agentos/core:latest
    container_name: agentos
    restart: unless-stopped
    ports:
      - "127.0.0.1:8080:8080"    # Web UI — bound to localhost only
                                  # Use a reverse proxy (nginx/caddy) for external access
    volumes:
      - agentos_vault:/opt/agentos/vault       # Encrypted secrets
      - agentos_data:/opt/agentos/data         # Semantic + episodic memory
      - agentos_agents:/opt/agentos/agents     # Agent registry
      - agentos_logs:/opt/agentos/logs         # Audit log
      - ./tools:/opt/agentos/tools/user        # User-installed tools (bind mount)
      - ./pipelines:/opt/agentos/pipelines     # User pipelines (bind mount)
      - ./config/agentos.toml:/opt/agentos/config/agentos.toml:ro
    environment:
      - AGENTOS_LOG_LEVEL=info
      - AGENTOS_WEB_UI=true
      # NOTE: No API keys here — use: agentctl secret set
    security_opt:
      - no-new-privileges:true               # Prevent privilege escalation
      - seccomp:./config/seccomp-profile.json # Custom seccomp for the kernel
    cap_drop:
      - ALL                                  # Drop all Linux capabilities
    cap_add:
      - NET_BIND_SERVICE                     # Only if binding to port < 1024
    read_only: true                          # Root filesystem read-only
    tmpfs:
      - /tmp:size=256m,noexec               # Temp space (no exec)
    healthcheck:
      test: ["CMD", "/usr/bin/agentos-kernel", "--health"]
      interval: 30s
      timeout: 10s
      retries: 3
    depends_on:
      ollama:
        condition: service_healthy

  # ── Local LLM (Optional) ───────────────────────────────────────
  ollama:
    image: ollama/ollama:latest
    container_name: agentos-ollama
    restart: unless-stopped
    volumes:
      - ollama_models:/root/.ollama
    # Internal network only — not exposed to host
    expose:
      - "11434"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:11434/api/health"]
      interval: 30s
      timeout: 10s
      retries: 5
      start_period: 60s

  # ── Reverse Proxy (Optional but recommended) ───────────────────
  caddy:
    image: caddy:alpine
    container_name: agentos-proxy
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./config/Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      - agentos

# ── Volumes ────────────────────────────────────────────────────────
volumes:
  agentos_vault:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: /opt/agentos-data/vault     # Host path — back this up
  agentos_data:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: /opt/agentos-data/data
  agentos_agents:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: /opt/agentos-data/agents
  agentos_logs:
    driver: local
    driver_opts:
      type: none
      o: bind
      device: /opt/agentos-data/logs
  ollama_models:
  caddy_data:
  caddy_config:

# ── Networks ───────────────────────────────────────────────────────
networks:
  default:
    name: agentos-network
    driver: bridge
    ipam:
      config:
        - subnet: 172.20.0.0/16
```

### Caddyfile (Reverse Proxy)

```caddyfile
# Replace with your domain or use :8443 for local HTTPS
agentos.yourdomain.com {
    reverse_proxy agentos:8080

    # Basic auth for web UI (until AgentOS has native auth)
    basicauth {
        admin $2a$14$...  # bcrypt hash of your password
    }

    # TLS handled automatically by Caddy (Let's Encrypt)
    tls your@email.com

    # Security headers
    header {
        Strict-Transport-Security "max-age=31536000; includeSubDomains"
        X-Content-Type-Options "nosniff"
        X-Frame-Options "DENY"
    }
}
```

### Quick Start (One Command)

```bash
# Clone and start
git clone https://github.com/your-org/agentos
cd agentos
cp config/example.toml config/agentos.toml

# Create host directories for persistent data
mkdir -p /opt/agentos-data/{vault,data,agents,logs}

# Start
docker compose up -d

# Connect your first LLM (API key entered securely)
docker exec -it agentos agentctl agent connect \
  --provider anthropic \
  --model claude-sonnet-4 \
  --name "assistant"

# Run your first task
docker exec -it agentos agentctl task run \
  --agent assistant \
  "List all installed tools and describe what you can do"
```

---

## Stage 2 — Minimal Linux Base Image

The goal here is to make Linux **invisible**. The host OS becomes a single-purpose platform whose only job is to run AgentOS. No package manager, no shell for users, no unnecessary services.

### Custom Base with Alpine

Alpine Linux is 5MB and ships with musl libc — perfect for a minimal base. The custom image:

```dockerfile
# Minimal base: Alpine stripped to essentials
FROM alpine:3.19 AS minimal-base

# Remove package manager after installing what we need
RUN apk add --no-cache wasmtime sqlcipher libgcc ca-certificates && \
    rm -rf /var/cache/apk/* && \
    rm -f /usr/bin/apk && \       # Remove package manager
    rm -rf /etc/apk && \
    rm -f /bin/sh /bin/ash && \   # Remove shells (agents don't need them)
    rm -rf /usr/share/man && \
    rm -rf /usr/share/doc

# AgentOS is the only process — no init system needed
# Use tini as a minimal PID 1 to handle signals correctly
COPY --from=tini /tini /tini

FROM minimal-base
COPY --from=builder /build/release/agentos-kernel /usr/bin/
COPY --from=builder /build/tools/core/ /opt/agentos/tools/core/

ENTRYPOINT ["/tini", "--", "/usr/bin/agentos-kernel"]
```

### Target: Single-Purpose Linux Boot

Going further, use a **custom Linux kernel** compiled with only the modules AgentOS needs:

```
Custom Linux kernel config (stripped down):
─────────────────────────────────────────────────────
Enabled:
  - x86_64 architecture support
  - ext4 filesystem
  - overlay filesystem (for container layers)
  - virtio drivers (for VMs)
  - NVMe / SATA drivers
  - Network drivers (e1000e, virtio-net)
  - Namespaces (for tool sandboxing)
  - seccomp (for tool syscall filtering)
  - cgroups v2 (for resource quotas)
  - KVM (if running VMs inside AgentOS)
  - NVIDIA / AMD GPU drivers (optional)

Disabled:
  - Bluetooth
  - USB audio
  - Floppy disk
  - PCMCIA
  - Amateur radio
  - All display drivers (no GUI)
  - IPX / AppleTalk networking
  - Everything else not needed
```

This produces a Linux kernel around **3–4MB** compared to the standard 10–12MB. Combined with Alpine, the entire host layer is under 10MB.

### Image Size Target (Stage 2)

| Component | Size |
|---|---|
| Custom Alpine base | ~3 MB |
| Custom Linux kernel | ~4 MB |
| AgentOS kernel binary | ~15 MB |
| Wasmtime | ~20 MB |
| Core tools | ~10 MB |
| SQLCipher | ~3 MB |
| **Total** | **~55 MB** |

---

## Stage 3 — Bare Metal Rust Kernel

This is the long-term vision: AgentOS boots directly on hardware with no Linux underneath. This is a multi-year effort but architecturally achievable in Rust.

### Why It's Possible in Rust

Rust's `no_std` mode allows writing code that runs without an operating system. Projects like **Redox OS** (a full OS written in Rust) and **Theseus OS** (a research OS in Rust) have proven this is viable. AgentOS would follow the same path.

### Boot Sequence (Bare Metal)

```
Power On
    │
    ▼
UEFI Firmware
    │
    ▼
AgentOS Bootloader (Rust, ~50KB)
  - Reads kernel from EFI System Partition
  - Sets up initial page tables
  - Switches to 64-bit long mode
    │
    ▼
AgentOS Kernel Entry Point (Rust, no_std)
  - Initializes memory allocator (buddy allocator)
  - Initializes hardware (PCI bus scan, NVMe, network)
  - Sets up interrupt handlers
  - Initializes scheduler
    │
    ▼
AgentOS HAL Initialization
  - Detects available hardware
  - Registers device drivers
  - Initializes GPU if present
    │
    ▼
AgentOS Runtime Start
  - Loads secrets vault
  - Loads agent registry
  - Starts agentd supervisor
  - Opens Intent Channels
  - Agents begin execution
```

### Kernel Architecture (no_std)

```rust
// No standard library — everything is explicit
#![no_std]
#![no_main]

use agentos_kernel::{
    memory::BuddyAllocator,
    scheduler::InferenceScheduler,
    hal::HardwareAbstractionLayer,
    vault::SecretsVault,
    agents::AgentRegistry,
};

#[no_mangle]
pub extern "C" fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // Initialize memory
    let allocator = BuddyAllocator::init(boot_info.memory_map);

    // Initialize hardware
    let hal = HardwareAbstractionLayer::init();

    // Initialize kernel subsystems
    let vault = SecretsVault::load_or_create(&hal.nvme);
    let registry = AgentRegistry::load(&hal.nvme);
    let scheduler = InferenceScheduler::new();

    // Start the AgentOS runtime
    agentos_runtime::start(vault, registry, scheduler, hal);

    // Never returns
    loop { core::hint::spin_loop() }
}
```

### Bootable Media

Once the bare metal kernel is built, AgentOS can be distributed as:

- **ISO image** — burn to USB, boot on any x86_64 machine
- **UEFI application** — install to EFI partition alongside other OS
- **VM image** — `.qcow2` or `.vmdk` for VMware / QEMU / VirtualBox
- **Cloud image** — AMI (AWS), custom image (GCP/Azure) without Linux base
- **Embedded image** — for ARM targets (Raspberry Pi, industrial hardware)

### Timeline Estimate

| Milestone | Estimated Effort |
|---|---|
| Basic bootloader (boots to Rust entry point) | 2–3 months |
| Memory management (allocator, page tables) | 2–3 months |
| Basic hardware init (PCI, NVMe, network) | 3–4 months |
| Port AgentOS runtime to no_std | 4–6 months |
| Stable bare metal build | 12–18 months total |

---

## Host Requirements

### Minimum (Development / Single Agent)

| Resource | Minimum | Recommended |
|---|---|---|
| CPU | 2 cores, x86_64 | 4 cores |
| RAM | 2 GB | 8 GB |
| Storage | 20 GB SSD | 50 GB SSD |
| Network | 10 Mbps | 100 Mbps |
| OS (Stage 1) | Any Linux with Docker | Ubuntu 22.04 LTS / Alpine |
| Docker | 24.0+ | Latest stable |

### Recommended (Production, Multiple Agents)

| Resource | Spec |
|---|---|
| CPU | 8+ cores, x86_64 or ARM64 |
| RAM | 32 GB (16 GB for agents + 16 GB for local LLM) |
| Storage | 200 GB NVMe SSD |
| Network | 1 Gbps |
| GPU (optional) | NVIDIA RTX 3080+ or A10G (for local inference) |
| OS (Stage 1) | Ubuntu 22.04 LTS or Alpine Linux 3.19 |

### For Local LLM (Ollama)

If running models locally instead of using API-based LLMs:

| Model Size | RAM Required | GPU VRAM | Storage |
|---|---|---|---|
| 7B (llama3.2) | 8 GB | 6 GB (optional) | 5 GB |
| 13B | 16 GB | 10 GB (optional) | 9 GB |
| 70B | 64 GB | 40 GB (required for speed) | 40 GB |

---

## Cloud Deployment

### AWS

```
Recommended Instance Types:
─────────────────────────────────────────────────
API-only (no local LLM):   t3.medium (2 vCPU, 4GB RAM)   ~$30/mo
With local 7B model:       t3.xlarge (4 vCPU, 16GB RAM)  ~$120/mo
With GPU inference:        g4dn.xlarge (1x T4 GPU)        ~$380/mo
Production multi-agent:    c5.2xlarge (8 vCPU, 16GB RAM) ~$250/mo
```

```bash
# Launch EC2 instance (Ubuntu 22.04)
aws ec2 run-instances \
  --image-id ami-0c55b159cbfafe1f0 \
  --instance-type t3.medium \
  --key-name your-keypair \
  --security-groups agentos-sg \
  --block-device-mappings '[{"DeviceName":"/dev/sda1","Ebs":{"VolumeSize":50,"VolumeType":"gp3"}}]'

# SSH in and install
ssh ubuntu@<ip>
curl -fsSL https://get.docker.com | sh
git clone https://github.com/your-org/agentos
cd agentos && docker compose up -d
```

**Storage:**
- Use **EBS gp3** volumes for the AgentOS data directory — fast, cheap, resizable
- Mount vault, data, and logs on separate EBS volumes for isolation
- Enable EBS encryption for the vault volume

**Security Groups (Firewall):**
```
Inbound:
  - Port 443 (HTTPS) from 0.0.0.0/0    ← Web UI via Caddy
  - Port 22 (SSH) from your IP only    ← Admin access

Outbound:
  - Port 443 to 0.0.0.0/0              ← LLM API calls
  - Port 11434 to AgentOS subnet only  ← Ollama (internal)
```

### GCP

```bash
# Create VM
gcloud compute instances create agentos \
  --machine-type=e2-standard-4 \
  --image-family=ubuntu-2204-lts \
  --image-project=ubuntu-os-cloud \
  --boot-disk-size=50GB \
  --boot-disk-type=pd-ssd \
  --zone=us-central1-a

# With GPU
gcloud compute instances create agentos-gpu \
  --machine-type=n1-standard-8 \
  --accelerator=type=nvidia-tesla-t4,count=1 \
  --image-family=ubuntu-2204-lts \
  --image-project=ubuntu-os-cloud \
  --maintenance-policy=TERMINATE
```

### Fly.io (Simplest Cloud Option)

Fly.io is the easiest deployment for developers — runs Docker containers globally:

```toml
# fly.toml
app = "my-agentos"
primary_region = "iad"

[build]
  image = "agentos/core:latest"

[mounts]
  source = "agentos_data"
  destination = "/opt/agentos/data"

[mounts]
  source = "agentos_vault"
  destination = "/opt/agentos/vault"

[[services]]
  internal_port = 8080
  protocol = "tcp"

  [[services.ports]]
    port = 443
    handlers = ["tls", "http"]

[env]
  AGENTOS_LOG_LEVEL = "info"

[[vm]]
  cpu_kind = "shared"
  cpus = 2
  memory_mb = 2048
```

```bash
fly launch
fly volumes create agentos_data --size 20
fly volumes create agentos_vault --size 1
fly deploy
```

### DigitalOcean (Best Value for Self-Hosters)

```bash
# Create a droplet via CLI
doctl compute droplet create agentos \
  --size s-2vcpu-4gb \            # $24/mo
  --image ubuntu-22-04-x64 \
  --region nyc3 \
  --ssh-keys your-key-id

# One-line install script (to be built)
ssh root@<ip> "curl -fsSL https://install.agentos.dev | bash"
```

---

## Self-Hosted / On-Premise Deployment

### Single Server

The most common deployment for individuals and small teams. One server running AgentOS, connected to cloud LLM APIs or a local Ollama instance.

```
[Your Server]
├── Ubuntu 22.04 LTS (host)
├── Docker 24.0
└── AgentOS Container
    ├── Vault (encrypted, backed up)
    ├── Semantic Store
    ├── Tool Registry
    └── Connected LLMs:
        ├── Anthropic API (cloud)
        ├── OpenAI API (cloud)
        └── Ollama/llama3.2 (local)
```

### Homelab / NAS Deployment

For users running Unraid, TrueNAS, or a Proxmox homelab:

```yaml
# Unraid Community Applications template
# Or run directly:

docker run -d \
  --name agentos \
  --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  -v /mnt/user/appdata/agentos/vault:/opt/agentos/vault \
  -v /mnt/user/appdata/agentos/data:/opt/agentos/data \
  -v /mnt/user/appdata/agentos/agents:/opt/agentos/agents \
  -v /mnt/user/appdata/agentos/logs:/opt/agentos/logs \
  --security-opt no-new-privileges:true \
  --cap-drop ALL \
  agentos/core:latest
```

### Air-Gapped Deployment (No Internet)

For secure environments with no external network access (local LLMs only):

```bash
# On an internet-connected machine:
docker pull agentos/core:latest
docker pull ollama/ollama:latest
docker save agentos/core:latest | gzip > agentos.tar.gz
docker save ollama/ollama:latest | gzip > ollama.tar.gz

# Transfer to air-gapped machine via USB/internal network
# On the air-gapped machine:
docker load < agentos.tar.gz
docker load < ollama.tar.gz

# Pre-download models on an internet machine:
ollama pull llama3.2
# Export and transfer model files

# Start with network disabled
docker compose up -d
# Configure only Ollama adapter (no cloud API keys needed)
agentctl agent connect --provider ollama --model llama3.2 --name "local-agent"
```

---

## Edge & Embedded Deployment

AgentOS's small footprint makes it suitable for edge computing and IoT scenarios where agents need to interact directly with local hardware.

### Raspberry Pi 4 / 5

```bash
# Requirements: Pi 4 with 8GB RAM minimum, NVMe hat recommended
# Install Raspberry Pi OS Lite (64-bit, no desktop)

# Install Docker
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker pi

# Pull ARM64 image
docker pull agentos/core:arm64

# Start with hardware access for GPIO
docker run -d \
  --name agentos \
  --restart unless-stopped \
  --device /dev/gpiomem \          # GPIO access
  --device /dev/i2c-1 \           # I2C sensors
  -v agentos_data:/opt/agentos/data \
  -v agentos_vault:/opt/agentos/vault \
  agentos/core:arm64

# Connect a local model via Ollama
# (Pi 4 8GB can run 3B-7B models slowly, Pi 5 is better)
```

### NVIDIA Jetson (Edge AI)

Jetson devices have onboard GPU — perfect for AgentOS with local inference:

```bash
# Jetson runs JetPack (Ubuntu-based) with CUDA support
docker pull agentos/core:jetson

docker run -d \
  --name agentos \
  --runtime nvidia \               # NVIDIA container runtime
  --gpus all \
  -e NVIDIA_VISIBLE_DEVICES=all \
  -v agentos_data:/opt/agentos/data \
  -v agentos_vault:/opt/agentos/vault \
  agentos/core:jetson

# Jetson Orin can run 7B-13B models at useful speeds
# Grant GPU access to specific agents:
agentctl perm grant local-agent hardware.gpu:rx
```

### Industrial / Embedded (Stage 3 Target)

Once the bare metal kernel is built, AgentOS can run directly on:
- Industrial PCs (x86_64, fanless)
- ARM Cortex-A based SBCs
- RISC-V development boards

---

## Networking Architecture

### Internal Network (Container-level)

```
┌─────────────────────────────────────────────────────┐
│              agentos-network (172.20.0.0/16)         │
│                                                     │
│  agentos     172.20.0.2   ←→   ollama  172.20.0.3   │
│     │                                               │
│     └──→ caddy  172.20.0.4  ←→  [external]          │
│                                                     │
└─────────────────────────────────────────────────────┘

External access:
  HTTPS :443 → Caddy → AgentOS Web UI :8080
  
Internal (never exposed):
  AgentOS Kernel API :9090 (Unix socket or internal only)
  Ollama API :11434 (internal network only)
```

### Ports Reference

| Port | Service | Exposure |
|---|---|---|
| 8080 | AgentOS Web UI | Internal only (proxy via Caddy/nginx) |
| 9090 | Kernel API | Internal only (never exposed) |
| 11434 | Ollama | Internal only (never exposed) |
| 443 | Caddy HTTPS | Public (Web UI access) |
| 80 | Caddy HTTP | Public (redirects to HTTPS) |

### Firewall Rules (ufw)

```bash
# On the host server
ufw default deny incoming
ufw default allow outgoing
ufw allow ssh                   # Your admin access
ufw allow 443/tcp               # Web UI (HTTPS)
ufw allow 80/tcp                # HTTP (redirects to HTTPS)
ufw deny 8080/tcp               # Never expose directly
ufw deny 9090/tcp               # Never expose directly
ufw enable
```

---

## Storage Architecture

### Volume Layout

```
Host Filesystem
└── /opt/agentos-data/
    ├── vault/              # CRITICAL — back up daily
    │   └── vault.db        # AES-256-GCM encrypted (SQLCipher)
    ├── data/
    │   ├── semantic/       # Vector store — large, grows over time
    │   │   └── qdrant/     # Or embedded vector DB
    │   └── episodic/       # Per-task SQLite files
    │       ├── task-001.db
    │       ├── task-002.db
    │       └── ...
    ├── agents/
    │   └── registry.db     # Agent profiles + permission matrices
    ├── logs/
    │   └── audit.log       # Append-only — never modified
    └── tools/
        └── user/           # User-installed tools
```

### Backup Strategy

```bash
#!/bin/bash
# backup-agentos.sh — run daily via cron

BACKUP_DIR="/backup/agentos/$(date +%Y-%m-%d)"
mkdir -p $BACKUP_DIR

# Stop container briefly for consistent snapshot (or use online backup)
docker stop agentos

# Critical: vault (encrypted, safe to store anywhere)
cp -r /opt/agentos-data/vault $BACKUP_DIR/vault

# Agent registry (permissions, profiles)
cp /opt/agentos-data/agents/registry.db $BACKUP_DIR/registry.db

# Audit log (append-only, important for compliance)
cp /opt/agentos-data/logs/audit.log $BACKUP_DIR/audit.log

# Resume
docker start agentos

# Sync to remote (vault is already encrypted)
rsync -az $BACKUP_DIR user@backup-server:/backups/agentos/

# Keep 30 days of backups
find /backup/agentos/ -type d -mtime +30 -exec rm -rf {} +

echo "Backup complete: $BACKUP_DIR"
```

### Storage Sizing Guide

| Component | Initial Size | Growth Rate |
|---|---|---|
| Vault | < 1 MB | Minimal |
| Agent registry | < 10 MB | Minimal |
| Episodic memory | 100 MB | ~10 MB per 1000 tasks |
| Semantic store | 500 MB | ~100 MB per 10,000 entries |
| Audit log | 10 MB | ~50 MB/month active use |
| Installed tools | 50 MB | Grows with tool installs |

---

## GPU Deployment

### NVIDIA (Recommended)

```bash
# Install NVIDIA Container Toolkit on host
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | \
  sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg

distribution=$(. /etc/os-release;echo $ID$VERSION_ID)
curl -s -L https://nvidia.github.io/libnvidia-container/$distribution/libnvidia-container.list | \
  sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list

sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker

# Verify GPU is visible
docker run --rm --gpus all nvidia/cuda:12.0-base nvidia-smi
```

```yaml
# docker-compose.yml with GPU
services:
  agentos:
    image: agentos/core:cuda
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: all              # Or specify: count: 1
              capabilities: [gpu, compute, utility]
    environment:
      - NVIDIA_VISIBLE_DEVICES=all
      - CUDA_VISIBLE_DEVICES=0       # Restrict to first GPU if multiple

  ollama:
    image: ollama/ollama:latest
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: 1
              capabilities: [gpu]
```

### Apple Silicon (Metal)

```bash
# macOS with Docker Desktop (Apple Silicon)
# Metal GPU passthrough is not yet supported in Docker for Mac
# Use native macOS deployment instead:

# Install directly (Stage 2 style, no container)
brew install agentos      # (when homebrew tap is available)
agentos start

# Ollama on Apple Silicon has native Metal acceleration
brew install ollama
ollama pull llama3.2
```

### GPU Sharing Between Agents

The GPU Manager allocates VRAM per task execution. Configuration:

```toml
# agentos.toml
[gpu]
enabled           = true
default_backend   = "auto"       # cuda | metal | vulkan | rocm
max_vram_percent  = 80           # Reserve 20% for system use
allocation_policy = "fair"       # fair | priority | first-come

# Per-agent GPU quota (enforced by kernel)
[gpu.quotas]
"agent:analyst"    = "2048mb"    # Max VRAM for this agent's tools
"agent:researcher" = "1024mb"
default            = "512mb"
```

---

## High Availability & Scaling

### Single Node (Most Users)

One container, one server. Simple, reliable, sufficient for most use cases. AgentOS is designed to be efficient — a single node can run dozens of concurrent agents.

### Multi-Node with Shared Storage

For teams needing redundancy:

```
                    ┌─────────────────┐
                    │  Load Balancer  │
                    │  (Caddy/nginx)  │
                    └────────┬────────┘
                             │
              ┌──────────────┴──────────────┐
              │                             │
    ┌─────────▼────────┐         ┌──────────▼───────┐
    │  AgentOS Node 1  │         │  AgentOS Node 2  │
    │  (Primary)       │         │  (Secondary)     │
    └─────────┬────────┘         └──────────┬───────┘
              │                             │
              └──────────────┬──────────────┘
                             │
                   ┌─────────▼──────────┐
                   │   Shared Storage   │
                   │  (NFS / Longhorn)  │
                   │  - Vault          │
                   │  - Semantic Store │
                   │  - Agent Registry │
                   └────────────────────┘
```

### Kubernetes Deployment

For large organizations managing agent fleets:

```yaml
# agentos-deployment.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: agentos
  namespace: agentos
spec:
  replicas: 1                # Start with 1 — AgentOS is stateful
  selector:
    matchLabels:
      app: agentos
  template:
    metadata:
      labels:
        app: agentos
    spec:
      securityContext:
        runAsNonRoot: true
        runAsUser: 1000
        runAsGroup: 1000
        fsGroup: 1000
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: agentos
          image: agentos/core:latest
          ports:
            - containerPort: 8080
          volumeMounts:
            - name: vault
              mountPath: /opt/agentos/vault
            - name: data
              mountPath: /opt/agentos/data
            - name: agents
              mountPath: /opt/agentos/agents
            - name: logs
              mountPath: /opt/agentos/logs
          resources:
            requests:
              memory: "2Gi"
              cpu: "1"
            limits:
              memory: "8Gi"
              cpu: "4"
          livenessProbe:
            httpGet:
              path: /health
              port: 8080
            initialDelaySeconds: 30
            periodSeconds: 30
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
      volumes:
        - name: vault
          persistentVolumeClaim:
            claimName: agentos-vault-pvc
        - name: data
          persistentVolumeClaim:
            claimName: agentos-data-pvc
        - name: agents
          persistentVolumeClaim:
            claimName: agentos-agents-pvc
        - name: logs
          persistentVolumeClaim:
            claimName: agentos-logs-pvc
---
apiVersion: v1
kind: Service
metadata:
  name: agentos
  namespace: agentos
spec:
  selector:
    app: agentos
  ports:
    - port: 80
      targetPort: 8080
  type: ClusterIP
```

---

## Security Hardening for Deployment

### Host-Level Hardening

```bash
# 1. Keep host OS updated
apt update && apt upgrade -y
apt install -y unattended-upgrades
dpkg-reconfigure --priority=low unattended-upgrades

# 2. Disable unnecessary services
systemctl disable bluetooth
systemctl disable cups
systemctl mask ctrl-alt-del.target

# 3. Configure SSH hardening
cat >> /etc/ssh/sshd_config << EOF
PermitRootLogin no
PasswordAuthentication no
PubkeyAuthentication yes
MaxAuthTries 3
ClientAliveInterval 300
ClientAliveCountMax 2
EOF
systemctl restart sshd

# 4. Set up fail2ban
apt install -y fail2ban
systemctl enable fail2ban

# 5. Configure kernel parameters
cat >> /etc/sysctl.conf << EOF
# Disable IP forwarding (unless needed for container networking)
net.ipv4.ip_forward = 0
# Prevent SYN flood attacks
net.ipv4.tcp_syncookies = 1
# Restrict ptrace (prevents process inspection)
kernel.yama.ptrace_scope = 1
# Disable ICMP redirects
net.ipv4.conf.all.accept_redirects = 0
EOF
sysctl -p
```

### Docker Daemon Hardening

```json
// /etc/docker/daemon.json
{
  "icc": false,                    // Disable inter-container communication by default
  "live-restore": true,            // Keep containers running during daemon updates
  "log-driver": "json-file",
  "log-opts": {
    "max-size": "100m",
    "max-file": "3"
  },
  "no-new-privileges": true,       // Default: prevent privilege escalation
  "userland-proxy": false,
  "seccomp-profile": "/etc/docker/seccomp.json",
  "userns-remap": "default"        // User namespace remapping
}
```

### Secrets Never In:

```
✗  docker-compose.yml environment section
✗  .env files committed to git
✗  Dockerfile ENV or ARG
✗  Kubernetes ConfigMaps (use Secrets with encryption at rest)
✗  Cloud provider environment variables in plain text
✗  Shell history (always use agentctl secret set interactive prompt)

✓  AgentOS Secrets Vault (AES-256-GCM, SQLCipher)
✓  Kubernetes Secrets with encryption at rest + RBAC
✓  AWS Secrets Manager / GCP Secret Manager (for cloud deployments)
✓  HashiCorp Vault (for enterprise deployments)
```

---

## Upgrade & Migration Strategy

### Version Upgrades

```bash
# Pull new image
docker pull agentos/core:latest

# Backup first (always)
./backup-agentos.sh

# Stop, replace, restart
docker compose down
docker compose up -d

# Verify health
docker exec agentos agentctl status
docker exec agentos agentctl audit logs --last 10
```

### Database Migrations

AgentOS uses versioned migrations for its internal databases:

```
/opt/agentos/data/
└── schema_version              # Tracks current schema version

On startup, AgentOS kernel:
  1. Reads current schema version
  2. Compares to expected version for this binary
  3. Runs pending migrations in order (never destructive)
  4. Updates schema version
  5. Starts normally
```

### Rollback

```bash
# Rollback to previous version (data is forward-compatible)
docker compose down
docker tag agentos/core:latest agentos/core:rollback-candidate
docker pull agentos/core:v1.2.3  # Previous known-good version
docker compose up -d
```

---

## Monitoring & Observability

### Built-in Health Endpoints

```
GET /health          → Kernel status, agent count, tool count
GET /metrics         → Prometheus-format metrics
GET /audit           → Audit log stream (authenticated)
```

### Prometheus + Grafana

```yaml
# Add to docker-compose.yml
  prometheus:
    image: prom/prometheus:latest
    volumes:
      - ./config/prometheus.yml:/etc/prometheus/prometheus.yml
    ports:
      - "127.0.0.1:9091:9090"

  grafana:
    image: grafana/grafana:latest
    ports:
      - "127.0.0.1:3000:3000"
    volumes:
      - grafana_data:/var/lib/grafana
```

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'agentos'
    static_configs:
      - targets: ['agentos:8080']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

### Key Metrics to Monitor

| Metric | Description | Alert Threshold |
|---|---|---|
| `agentos_tasks_active` | Currently running tasks | > 50 |
| `agentos_tasks_failed_total` | Cumulative failed tasks | Rate > 5/min |
| `agentos_llm_latency_p99` | LLM call latency | > 30s |
| `agentos_vault_access_total` | Secret retrievals | Unexpected spike |
| `agentos_permission_denied_total` | Blocked intents | > 10/min |
| `agentos_tool_errors_total` | Tool execution errors | Rate > 1/min |
| `agentos_memory_semantic_size` | Semantic store size | > 10GB |
| `agentos_audit_log_size` | Audit log size | > 5GB |

---

## Security Gate — Required before launch

**All 7 security acceptance scenarios must pass before any deployment.** This is a hard gate — no exceptions.

### Run the suite

```bash
cargo test -p agentos-kernel --test security_acceptance_test
```

Expected result: `test result: ok. 7 passed; 0 failed; 0 ignored`

### Scenario checklist

| # | Scenario | Pass? |
|---|----------|-------|
| A | Unsigned A2A message rejected | must be ✓ |
| B | Forged Ed25519 signature rejected | must be ✓ |
| C | Secret scope denial enforced (Agent B denied Agent A's secret) | must be ✓ |
| D | High-risk (`Delegate`) intent requires hard approval — task enters `PendingEscalation` | must be ✓ |
| E | Prompt injection payloads detected and flagged by `InjectionScanner` | must be ✓ |
| F | `Blocked`-tier tool registration fails with `ToolBlocked` error | must be ✓ |
| G | Community tool with invalid Ed25519 signature fails with `ToolSignatureInvalid` | must be ✓ |

### Policy

- Any scenario failure is a **hard deployment block** — resolve before proceeding.
- After any change to `agent_message_bus.rs`, `vault.rs`, `risk_classifier.rs`, `injection_scanner.rs`, or `tool_registry.rs`, re-run the suite.
- The audit log must contain corresponding security events for each triggered scenario.

---

## Deployment Checklist

### Before First Launch

- [ ] Host server provisioned with adequate CPU/RAM/storage
- [ ] Docker installed and running
- [ ] Host firewall configured (only 80/443 open)
- [ ] Host SSH hardened (key auth only, root login disabled)
- [ ] Persistent volume directories created with correct permissions
- [ ] `agentos.toml` configuration reviewed and customized
- [ ] Reverse proxy (Caddy/nginx) configured with HTTPS
- [ ] Backup script created and tested
- [ ] Cron job for automated backups configured

### After First Launch

- [ ] `agentctl status` returns healthy
- [ ] Web UI accessible via HTTPS
- [ ] First agent connected via `agentctl agent connect` (interactive key prompt)
- [ ] Test task runs successfully
- [ ] Audit log shows task activity
- [ ] Secrets list shows credential (name only, not value)
- [ ] Permission matrix reviewed for each agent

### Before Production Use

- [ ] All agents have minimal permissions (principle of least privilege)
- [ ] Hardware permissions reviewed — denied unless explicitly needed
- [ ] Backup tested by restoring to a second server
- [ ] Monitoring/alerting configured
- [ ] Upgrade procedure documented and tested
- [ ] Incident response plan: what to do if an agent behaves unexpectedly

---

*AgentOS Deployment Guide — last updated with Stage 1 (Docker), Stage 2 (Minimal Linux), and Stage 3 (Bare Metal) architecture.*

> **Current target**: Stage 1 (Docker on Linux)
> **Stage 2 target**: 6–12 months post-launch
> **Stage 3 target**: 18–36 months post-launch
