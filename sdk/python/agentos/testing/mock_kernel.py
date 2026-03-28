"""
MockKernel — in-process mock for unit testing Python tools and agents.

Does not require a running Rust kernel. All command handling is done
in-process by a configurable response registry.

Usage:
    async with MockKernel() as kernel:
        kernel.set_llm_response("42")
        agent = await kernel.connect_agent("test-agent")
        result = await agent.run("What is 6 * 7?")
        assert result.success
        assert "42" in result.output
"""
from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field
from typing import Any

from ..agent import Agent
from ..client import BusClient
from ..exceptions import KernelCommandError


@dataclass
class _QueuedNotification:
    id: str
    kind: str
    subject: str
    body: str
    priority: str = "info"
    read: bool = False
    created_at: str = ""
    from_agent_id: str | None = None


class MockBusClient(BusClient):
    """
    Drop-in replacement for BusClient that routes commands to MockKernel
    without opening a real socket.
    """

    def __init__(self, kernel: "MockKernel") -> None:
        # Don't call super().__init__ — we don't need socket state.
        self._kernel = kernel
        self._reader = None  # type: ignore[assignment]
        self._writer = None  # type: ignore[assignment]

    async def connect(self) -> None:  # noqa: D102
        pass  # No-op — no real socket

    async def close(self) -> None:  # noqa: D102
        pass  # No-op

    async def send_command(self, command_variant: str, **fields: Any) -> dict[str, Any]:
        """Dispatch command to MockKernel handler."""
        return await self._kernel._dispatch(command_variant, **fields)


class MockKernel:
    """
    In-process mock of the AgentOS kernel for unit testing.

    Handlers registered via add_command_handler() take priority. If no
    handler is registered, built-in defaults handle common commands.
    """

    def __init__(self) -> None:
        self._llm_response: str = "mock response"
        self._tool_responses: dict[str, Any] = {}
        self._command_handlers: dict[str, Any] = {}
        self._agents: dict[str, str] = {}  # name → agent_id
        self._tasks: list[dict[str, Any]] = []
        self._notifications: list[_QueuedNotification] = []
        self._notification_responses: dict[str, str] = {}

    # ------------------------------------------------------------------
    # Configuration
    # ------------------------------------------------------------------

    def set_llm_response(self, response: str) -> None:
        """Set the text returned for all RunTask commands."""
        self._llm_response = response

    def add_tool_response(self, tool_name: str, response: Any) -> None:
        """Pre-configure the response for a specific tool invocation."""
        self._tool_responses[tool_name] = response

    def add_command_handler(
        self, command_variant: str, handler: Any
    ) -> None:
        """
        Register a custom async handler for a KernelCommand variant.

        The handler receives the same **fields as send_command() and must
        return a dict matching the KernelResponse wire format.
        """
        self._command_handlers[command_variant] = handler

    def push_notification(
        self,
        *,
        kind: str = "Notification",
        subject: str = "Test notification",
        body: str = "",
        priority: str = "info",
        notification_id: str | None = None,
    ) -> str:
        """
        Push a notification to the mock inbox.

        Returns the notification ID.
        """
        nid = notification_id or str(uuid.uuid4())
        self._notifications.append(
            _QueuedNotification(
                id=nid,
                kind=kind,
                subject=subject,
                body=body,
                priority=priority,
            )
        )
        return nid

    def get_notification_response(self, notification_id: str) -> str | None:
        """Return the response submitted for a notification, if any."""
        return self._notification_responses.get(notification_id)

    # ------------------------------------------------------------------
    # Agent factory
    # ------------------------------------------------------------------

    async def connect_agent(self, name: str, **_kwargs: Any) -> Agent:
        """Create an Agent backed by this MockKernel (no real socket)."""
        client = MockBusClient(self)
        agent_id = self._get_or_create_agent(name)
        return Agent(name=name, client=client, agent_id=agent_id)

    def _get_or_create_agent(self, name: str) -> str:
        if name not in self._agents:
            self._agents[name] = str(uuid.uuid4())
        return self._agents[name]

    # ------------------------------------------------------------------
    # Context manager
    # ------------------------------------------------------------------

    async def __aenter__(self) -> "MockKernel":
        return self

    async def __aexit__(self, *_: Any) -> None:
        pass

    # ------------------------------------------------------------------
    # Command dispatch
    # ------------------------------------------------------------------

    async def _dispatch(self, command_variant: str, **fields: Any) -> dict[str, Any]:
        if command_variant in self._command_handlers:
            return await self._command_handlers[command_variant](**fields)

        handler_name = f"_handle_{command_variant}"
        handler = getattr(self, handler_name, None)
        if handler is not None:
            return await handler(**fields)

        # Default: Success with empty data
        return {"Success": {"data": {}}}

    # ------------------------------------------------------------------
    # Built-in command handlers
    # ------------------------------------------------------------------

    async def _handle_ConnectAgent(self, name: str, **_: Any) -> dict[str, Any]:
        agent_id = self._get_or_create_agent(name)
        return {"Success": {"data": {"agent_id": agent_id}}}

    async def _handle_RunTask(self, prompt: str, **_: Any) -> dict[str, Any]:
        task_id = str(uuid.uuid4())
        self._tasks.append(
            {"task_id": task_id, "prompt": prompt, "result": self._llm_response}
        )
        return {
            "Success": {
                "data": {"task_id": task_id, "result": self._llm_response}
            }
        }

    async def _handle_ListTasks(self, **_: Any) -> dict[str, Any]:
        summaries = [
            {
                "id": t["task_id"],
                "state": "Complete",
                "agent_id": "",
                "prompt_preview": t["prompt"][:100],
                "created_at": "",
                "tool_calls": 0,
                "tokens_used": 0,
                "priority": 5,
            }
            for t in self._tasks
        ]
        return {"TaskList": summaries}

    async def _handle_ListNotifications(
        self, unread_only: bool = True, limit: int = 50, **_: Any
    ) -> dict[str, Any]:
        msgs = [
            {
                "id": n.id,
                # Match the real kernel's UserMessageKind internally-tagged serde format:
                # #[serde(tag = "kind")] -> {"kind": "Notification"} for unit variants,
                # {"kind": "Question", "question": "...", ...} for struct variants.
                "kind": {"kind": n.kind},
                "subject": n.subject,
                "body": n.body,
                "priority": n.priority,
                "read": n.read,
                "created_at": n.created_at,
                "from": {"type": "Kernel"},
                "trace_id": str(uuid.uuid4()),
                "delivery_status": {},
            }
            for n in self._notifications
            if not (unread_only and n.read)
        ]
        return {"NotificationList": msgs[:limit]}

    async def _handle_MarkNotificationRead(
        self, notification_id: str, **_: Any
    ) -> dict[str, Any]:
        for n in self._notifications:
            if n.id == notification_id:
                n.read = True
                break
        return {"Success": {"data": {}}}

    async def _handle_RespondToNotification(
        self, notification_id: str, response_text: str, **_: Any
    ) -> dict[str, Any]:
        self._notification_responses[notification_id] = response_text
        for n in self._notifications:
            if n.id == notification_id:
                n.read = True
                break
        return {"Success": {"data": {}}}

    async def _handle_GrantPermission(self, **_: Any) -> dict[str, Any]:
        return {"Success": {"data": {}}}

    async def _handle_RevokePermission(self, **_: Any) -> dict[str, Any]:
        return {"Success": {"data": {}}}

    async def _handle_ShowPermissions(self, **_: Any) -> dict[str, Any]:
        return {"Permissions": {"entries": []}}

    async def _handle_InstallTool(self, **_: Any) -> dict[str, Any]:
        return {"Success": {"data": {}}}

    async def _handle_DisconnectAgent(self, **_: Any) -> dict[str, Any]:
        return {"Success": {"data": {}}}
