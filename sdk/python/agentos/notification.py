"""
Notification inbox — mirrors the UNIS notification system in the kernel.

Kernel commands used:
  ListNotifications   { unread_only: bool, limit: u32 }
  MarkNotificationRead { notification_id: NotificationID }
  RespondToNotification { notification_id, response_text, channel }

Responses:
  NotificationList(Vec<UserMessage>)
  Success { data: None }
"""
from __future__ import annotations

import asyncio
from collections import deque
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, AsyncIterator

if TYPE_CHECKING:
    from .client import BusClient

from .exceptions import KernelConnectionError

# Maximum number of notification IDs kept in the deduplication set.
# Evicts oldest IDs once this limit is exceeded, bounding memory for long-running agents.
_SEEN_SET_MAX = 5_000


@dataclass
class Message:
    """
    Simplified view of a UserMessage (agentos_types::UserMessage).

    Only exposes the fields relevant for Python SDK consumers.
    """

    id: str
    """NotificationID (UUID string)."""

    kind: str
    """
    Message kind — one of: "Notification", "Question", "TaskComplete",
    "StatusUpdate". Derived from the UserMessageKind tag.
    """

    subject: str
    """Short summary ≤80 chars."""

    body: str
    """Full markdown body."""

    priority: str
    """one of: info, warning, urgent, critical"""

    read: bool
    created_at: str

    # Optional fields
    from_agent_id: str | None = None
    task_id: str | None = None
    question: str | None = None
    """Set when kind == 'Question'."""

    options: list[str] | None = None
    """Allowed choices when kind == 'Question' and options is non-null."""

    raw: dict[str, Any] = field(default_factory=dict)
    """Original wire dict, for advanced consumers."""

    @classmethod
    def from_wire(cls, d: dict[str, Any]) -> "Message":
        """Parse a UserMessage dict from the kernel wire format."""
        kind_raw = d.get("kind", {})
        if isinstance(kind_raw, str):
            kind_tag = kind_raw
            question = None
            options = None
        elif isinstance(kind_raw, dict):
            kind_tag = kind_raw.get("kind", "Notification")
            question = kind_raw.get("question")
            options = kind_raw.get("options")
        else:
            kind_tag = "Notification"
            question = None
            options = None

        from_raw = d.get("from", {})
        from_agent_id: str | None = None
        if isinstance(from_raw, dict) and from_raw.get("type") == "Agent":
            from_agent_id = from_raw.get("id")

        return cls(
            id=d.get("id", ""),
            kind=kind_tag,
            subject=d.get("subject", ""),
            body=d.get("body", ""),
            priority=d.get("priority", "info"),
            read=d.get("read", False),
            created_at=d.get("created_at", ""),
            from_agent_id=from_agent_id,
            task_id=d.get("task_id"),
            question=question,
            options=options,
            raw=d,
        )

    def is_question(self) -> bool:
        return self.kind == "Question"


class Inbox:
    """
    Agent notification inbox backed by the kernel's UNIS notification system.

    Supports both one-shot listing and async streaming (polling every 2 seconds).
    """

    def __init__(self, client: "BusClient", poll_interval: float = 2.0) -> None:
        self._client = client
        self._poll_interval = poll_interval

    async def messages(
        self, *, unread_only: bool = True, limit: int = 50
    ) -> list[Message]:
        """Fetch notifications from the kernel."""
        response = await self._client.send_command(
            "ListNotifications",
            unread_only=unread_only,
            limit=limit,
        )
        # Response is NotificationList(Vec<UserMessage>)
        # Wire: {"NotificationList": [...]}
        msgs_raw: list[dict[str, Any]] = []
        if isinstance(response, dict):
            msgs_raw = response.get("NotificationList", [])
        elif isinstance(response, list):
            msgs_raw = response

        return [Message.from_wire(m) for m in msgs_raw]

    async def mark_read(self, notification_id: str) -> None:
        """Mark a notification as read."""
        await self._client.send_command(
            "MarkNotificationRead",
            notification_id=notification_id,
        )

    async def respond(self, notification_id: str, text: str, channel: str = "cli") -> None:
        """
        Reply to a Question-kind notification, unblocking the waiting task.

        Args:
            notification_id: The UUID of the Question notification.
            text: The response text (free-form or one of the allowed options).
            channel: Delivery channel identifier (default "cli").
        """
        await self._client.send_command(
            "RespondToNotification",
            notification_id=notification_id,
            response_text=text,
            channel=channel,
        )

    async def __aiter__(self) -> AsyncIterator[Message]:
        """
        Async generator that yields new (unread) messages as they arrive.

        Polls every `poll_interval` seconds. Deduplicates by message ID so
        each message is yielded exactly once per iteration session.

        Raises KernelConnectionError if the kernel connection is lost.
        Other transient errors are retried silently.
        """
        seen: set[str] = set()
        # FIFO insertion-order tracker for bounded eviction
        seen_order: deque[str] = deque()

        while True:
            try:
                msgs = await self.messages(unread_only=True)
            except KernelConnectionError:
                raise  # Connection lost — propagate to caller
            except Exception:  # noqa: BLE001
                await asyncio.sleep(self._poll_interval)
                continue

            for msg in msgs:
                if msg.id not in seen:
                    seen.add(msg.id)
                    seen_order.append(msg.id)
                    # Evict oldest entry when set grows too large
                    if len(seen) > _SEEN_SET_MAX:
                        oldest = seen_order.popleft()
                        seen.discard(oldest)
                    yield msg

            await asyncio.sleep(self._poll_interval)
