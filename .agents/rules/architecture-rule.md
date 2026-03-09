---
trigger: always_on
glob: "**/*"
description: Architecture rules enforcing secure, efficient, well-structured applications.
---

# Architecture Rules

> **Mandatory for all applications, modules, components, and services.**

---

## 1. Project Structure

### Layout

```
project-root/
├── src/
│   ├── core/           # Business logic, domain models, pure functions
│   ├── services/       # Orchestrates core logic, external calls
│   ├── handlers/       # Route controllers, API endpoints
│   ├── middleware/      # Auth, logging, rate-limiting, validation
│   ├── models/         # Data models, schemas, type definitions
│   ├── repositories/   # Data access layer — DB queries, storage
│   ├── utils/          # Pure utility functions — no side effects
│   ├── config/         # Configuration loading, environment parsing
│   ├── errors/         # Custom error types, error codes
│   ├── types/          # Shared type definitions, interfaces, enums
│   └── main.{ext}      # Single entry point — minimal bootstrap only
├── tests/
│   ├── unit/
│   ├── integration/
│   └── e2e/
├── scripts/            # Build scripts, CI helpers, migrations
├── docs/               # ADRs, API specs
├── config/             # Environment-specific configs
├── .env.example        # Template — NEVER commit .env
└── README.md
```

### Conventions

- **Files**: `kebab-case` (exception: Python `snake_case`, frontend components `PascalCase`)
- **Modules**: No circular dependencies. Flow: `handlers → services → repositories → models/core`
- **Exports**: Each module has an `index`/`mod` file exposing only its public API
- **Types**: Shared types in `types/`, utilities in `utils/` — no duplication

---

## 2. Security — Non-Negotiable

### Secrets

- **NEVER** hardcode secrets, API keys, tokens, or passwords in source code
- **NEVER** commit `.env`, private keys, or credentials to version control
- Load secrets from environment variables or a secrets manager at runtime
- Validate all secrets at startup — fail fast if missing. No silent defaults
- Redact all secret values from logs, errors, and stack traces

### Input Validation

- **ALL external input is untrusted** — request bodies, params, headers, file uploads, WebSocket messages, env vars, third-party API data
- Validate at the boundary layer BEFORE passing to services/core
- Use schema validation (Zod, JSON Schema, serde, Pydantic) — not manual string checks
- Validate: type, length, charset, range, required fields
- Sanitize before use in SQL, HTML, shell commands, file paths, or logs
- **Parameterized queries only** — raw string SQL is FORBIDDEN

### Auth

- Authentication via middleware BEFORE any handler logic
- Use JWT (short expiry + refresh), OAuth 2.0/OIDC, or secure session cookies (`HttpOnly`, `Secure`, `SameSite=Strict`)
- Explicit authorization checks per-request. Never assume access
- Rate-limit auth endpoints. Log all failed attempts with timestamp, IP, and reason

### API Security

- HTTPS enforced in production. Security headers on all responses: `CSP`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `HSTS`, `Referrer-Policy`, `Permissions-Policy`
- CORS with explicit origins — never `*` in production
- Rate limiting per-user, per-IP, per-endpoint
- API errors return generic messages — never expose internals

### Dependencies & Data

- Pin all dependency versions. Audit for vulnerabilities before adding
- Minimize dependency tree — prefer stdlib solutions
- Encrypt sensitive data at rest. TLS 1.2+ for data in transit
- Hash passwords with `bcrypt`, `scrypt`, or `argon2id` — never MD5/SHA alone

---

## 3. Efficiency & Performance

### Resources

- All I/O MUST be async. Database connections MUST use pooling
- Properly close all handles (RAII, try-with-resources, context managers, `defer`)
- Explicit timeouts on ALL external calls. Circuit breakers for external services

### Caching

- Cache at appropriate level: in-memory (LRU/TTL), distributed (Redis), HTTP (Cache-Control/ETags)
- Every cache entry MUST have a TTL. Implement invalidation strategy

### Database

- Index columns used frequently in `WHERE` clauses with high selectivity
  - Use composite indexes for multi-column queries
  - Profile queries and add indexes based on actual performance data
  - Avoid indexing low-cardinality columns or small tables (write/space overhead)
- Paginate all list endpoints — unbounded queries FORBIDDEN
- Avoid N+1 queries. Use transactions for atomic multi-step operations

### Compute & Concurrency

- Stream large datasets — never load entirely into memory
- Use language-native concurrency (async/await, goroutines, tokio). Protect shared state with mutexes or prefer message-passing
- Bounded channels/queues to prevent unbounded growth

---

## 4. Error Handling

### Error Types

Define distinct types: `ValidationError`, `AuthenticationError`, `AuthorizationError`, `NotFoundError`, `ConflictError`, `ExternalServiceError`, `InternalError`. Each includes: error code, user message, internal detail (logs only), HTTP status mapping.

### Propagation

- Use native error mechanisms (`Result<T,E>`, `try/catch`, exceptions) — not error codes or nulls
- NEVER silently swallow errors — log, re-raise, or explicitly handle with documented reason
- Structured error responses: `{ "error": { "code": "...", "message": "...", "details": [...] } }`

### Resilience

- Degrade gracefully for non-critical failures. Health check endpoints (`/health`, `/ready`)
- Retry with exponential backoff + jitter, max retry count. Log degradations at `WARN`+

---

## 5. Observability

- **Structured logging** (JSON) — never `print`/`console.log`. Include: timestamp (ISO 8601 UTC), level, service, trace ID, message
- Levels: `ERROR` (failure), `WARN` (recoverable), `INFO` (business events), `DEBUG` (dev only)
- NEVER log credentials, tokens, PII. Log all requests, auth events, errors
- Generate unique trace ID at entry, propagate through all calls. Use OpenTelemetry for multi-service
- Expose metrics: request/error rate, latency p50/p95/p99, connections, queue depth

---

## 6. API Design

- HTTP methods: `GET`/`POST`/`PUT`/`PATCH`/`DELETE`. Plural nouns: `/users`, `/orders`
- Version from day one: `/api/v1/...`. Max 2 levels of nesting
- Correct status codes: `200`/`201`/`204`/`400`/`401`/`403`/`404`/`409`/`422`/`429`/`500`
- Explicit request/response schemas. ISO 8601 dates (UTC). Pagination metadata on all lists
- OpenAPI/Swagger docs for all endpoints, kept in sync with code

---

## 7. Testing

- **Unit**: All `core/` and `services/` logic — happy path, edge cases, errors
- **Integration**: All DB operations and external integrations
- **E2E**: Critical user flows. 100% coverage on security-critical paths
- Tests MUST be deterministic and isolated — no shared state, no flaky tests
- Descriptive names: `test_user_creation_fails_with_duplicate_email`

---

## 8. Configuration

- Externalize ALL config — never hardcode URLs, endpoints, flags, timeouts
- Validate at startup. Fail fast if required config missing
- Priority: env vars → environment config file → default config → hardcoded defaults (non-critical only)

---

## 9. Docker & Deployment

- Multi-stage builds. Specific base image tags — never `latest`. Non-root user
- `.dockerignore`: `.git`, `node_modules`, `target/`, `__pycache__`, tests, IDE configs
- `HEALTHCHECK` in Dockerfile. Graceful shutdown (SIGTERM handling)
- Production checklist: secrets in vault, HTTPS enforced, rate limiting, security headers, resource limits set

---

## 10. Code Quality

- Single Responsibility. Functions ≤40 lines. Composition over inheritance
- Descriptive names — no abbreviations (`usr`, `prvdr`). Comments explain **why**, not **what**
- Standard formatters (`rustfmt`, `prettier`, `black`, `gofmt`). Linter warnings = errors in CI
- Conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`

---

## 11. Frontend (When Applicable)

- Separate presentational vs container components. Max 200 lines per component
- Local state for UI-only; app-level state only for shared cross-component data
- Lazy-load routes, optimize images (WebP/AVIF), code-split by route
- Keyboard-navigable, semantic HTML, `aria-label`, WCAG AA contrast, `<label>` on all inputs

---

## Non-Negotiables

| Rule                              | Severity     |
| --------------------------------- | ------------ |
| No hardcoded secrets              | **CRITICAL** |
| All input validated/sanitized     | **CRITICAL** |
| Parameterized queries only        | **CRITICAL** |
| Auth checked per-request          | **CRITICAL** |
| Errors never expose internals     | **HIGH**     |
| Structured logging with trace IDs | **HIGH**     |
| Async I/O with timeouts           | **HIGH**     |
| Connection pooling                | **HIGH**     |
| No circular dependencies          | **HIGH**     |
| Unit tests for business logic     | **HIGH**     |

> **Prioritize security over convenience, correctness over speed, clarity over cleverness.**
