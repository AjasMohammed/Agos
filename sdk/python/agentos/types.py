"""
Python dataclasses mirroring the Rust serde types in agentos-types.

These are the wire-format types used when communicating with the kernel.
Serde defaults: enums are externally tagged {"VariantName": fields},
unit variants serialize as plain strings "VariantName".
"""
from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class LLMProvider(str, Enum):
    """
    Mirrors agentos_types::LLMProvider.

    Unit variants (Ollama, OpenAI, Anthropic, Gemini) serialize as plain
    strings in serde's default externally-tagged format.

    The Custom variant is NOT representable as a str enum because serde
    serializes it as {"Custom": "<endpoint_url>"}. Use LLMProvider.custom_wire()
    to build the correct JSON value for a custom provider.
    """

    Ollama = "Ollama"
    OpenAI = "OpenAI"
    Anthropic = "Anthropic"
    Gemini = "Gemini"

    def to_wire(self) -> Any:
        """Return the serde-compatible JSON value for this provider."""
        return self.value

    @staticmethod
    def custom_wire(endpoint_url: str) -> Any:
        """
        Build the wire value for a Custom LLM provider endpoint.

        Example:
            provider = LLMProvider.custom_wire("http://localhost:8080")
            # Pass as: provider=LLMProvider.custom_wire("http://...")
        """
        return {"Custom": endpoint_url}


class TaskState(str, Enum):
    """Mirrors agentos_types::TaskState."""

    Queued = "Queued"
    Running = "Running"
    Waiting = "Waiting"
    Suspended = "Suspended"
    Complete = "Complete"
    Failed = "Failed"
    Cancelled = "Cancelled"

    @property
    def is_terminal(self) -> bool:
        return self in (TaskState.Complete, TaskState.Failed, TaskState.Cancelled)


class NotificationPriority(str, Enum):
    """Mirrors agentos_types::NotificationPriority (serde rename_all = snake_case)."""

    Info = "info"
    Warning = "warning"
    Urgent = "urgent"
    Critical = "critical"


@dataclass
class TaskSummary:
    """Mirrors agentos_types::TaskSummary."""

    id: str
    state: TaskState
    agent_id: str
    prompt_preview: str
    created_at: str
    tool_calls: int
    tokens_used: int
    priority: int

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "TaskSummary":
        raw_state = d.get("state", "Complete")
        try:
            state = TaskState(raw_state)
        except ValueError:
            state = TaskState.Complete  # safe fallback for unknown states
        return cls(
            id=d.get("id", ""),
            state=state,
            agent_id=d.get("agent_id", ""),
            prompt_preview=d.get("prompt_preview", ""),
            created_at=d.get("created_at", ""),
            tool_calls=d.get("tool_calls", 0),
            tokens_used=d.get("tokens_used", 0),
            priority=d.get("priority", 5),
        )


@dataclass
class AgentProfile:
    """Mirrors agentos_types::AgentProfile (subset of fields)."""

    id: str
    name: str
    provider: str
    model: str
    status: str
    roles: list[str] = field(default_factory=list)
    current_task: str | None = None
    description: str = ""
    created_at: str = ""
    last_active: str = ""
    public_key_hex: str | None = None

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "AgentProfile":
        return cls(
            id=d["id"],
            name=d["name"],
            provider=d.get("provider", ""),
            model=d.get("model", ""),
            status=d.get("status", ""),
            roles=d.get("roles", []),
            current_task=d.get("current_task"),
            description=d.get("description", ""),
            created_at=d.get("created_at", ""),
            last_active=d.get("last_active", ""),
            public_key_hex=d.get("public_key_hex"),
        )
