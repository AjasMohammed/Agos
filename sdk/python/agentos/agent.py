"""
Agent — high-level interface for connecting to and interacting with AgentOS.

Usage:
    async with await Agent.connect("my-agent") as agent:
        result = await agent.run("What is 2 + 2?")
        print(result.output)
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from .client import BusClient
from .exceptions import KernelCommandError, TaskError
from .notification import Inbox
from .task import TaskResult
from .types import LLMProvider, TaskSummary


class Agent:
    """
    Represents a connected AgentOS agent.

    Wraps a BusClient and provides high-level methods for task submission,
    permission management, and notification access.
    """

    def __init__(self, name: str, client: BusClient, agent_id: str) -> None:
        self._name = name
        self._client = client
        self._agent_id = agent_id

    # ------------------------------------------------------------------
    # Factory / connect
    # ------------------------------------------------------------------

    @classmethod
    async def connect(
        cls,
        name: str,
        *,
        model: str = "claude-sonnet-4-6",
        provider: str | LLMProvider = LLMProvider.Anthropic,
        socket_path: Path | None = None,
        base_url: str | None = None,
        roles: list[str] | None = None,
        extra_permissions: list[str] | None = None,
    ) -> "Agent":
        """
        Connect to the kernel and register (or reconnect) an agent by name.

        Args:
            name: Agent name (alphanumeric, hyphens, underscores, dots; max 64 chars).
            model: LLM model identifier.
            provider: LLM provider (default Anthropic).
            socket_path: Override the default kernel socket path.
            base_url: Optional custom LLM endpoint URL.
            roles: Roles to assign on first connect. Default: ["general"].
            extra_permissions: Extra permissions to grant on connect
                (format: "resource:flags", e.g. "process.exec:x").

        Returns:
            Connected Agent instance.

        Raises:
            KernelConnectionError: If the kernel is not reachable.
            KernelCommandError: If the kernel rejects the connection.
        """
        provider_wire: Any
        if isinstance(provider, LLMProvider):
            provider_wire = provider.value
        elif isinstance(provider, str):
            provider_wire = provider
        else:
            provider_wire = str(provider)

        client = BusClient(socket_path or BusClient.DEFAULT_SOCKET)
        await client.connect()

        try:
            fields: dict[str, Any] = {
                "name": name,
                "provider": provider_wire,
                "model": model,
                "base_url": base_url,
                "roles": roles or [],
                "test_mode": False,
                "extra_permissions": extra_permissions or [],
            }
            response = await client.send_command("ConnectAgent", **fields)

            # Extract agent_id from Success { data: {"agent_id": "..."} }
            agent_id = _extract_agent_id(response)
            return cls(name=name, client=client, agent_id=agent_id)
        except Exception:
            await client.close()
            raise

    # ------------------------------------------------------------------
    # Task submission
    # ------------------------------------------------------------------

    async def run(
        self,
        prompt: str,
        *,
        autonomous: bool = False,
    ) -> TaskResult:
        """
        Submit a task and wait for the kernel to complete it.

        The kernel executes RunTask synchronously and returns the result
        in the same response. This call blocks until the task completes.

        Args:
            prompt: The task description / instruction.
            autonomous: Run without iteration/timeout limits. Default False.

        Returns:
            TaskResult with the agent's output.

        Raises:
            TaskError: If the kernel returns a task error.
            KernelCommandError: If the kernel returns an unexpected error.
        """
        try:
            response = await self._client.send_command(
                "RunTask",
                agent_name=self._name,
                prompt=prompt,
                autonomous=autonomous,
            )
        except KernelCommandError as exc:
            raise TaskError(str(exc)) from exc

        # Response: Success { data: {"task_id": "...", "result": "..."} }
        data = _unwrap_success(response)
        return TaskResult.from_success_data(data)

    async def run_background(self, prompt: str, *, autonomous: bool = False) -> str:
        """
        Submit a task and return immediately with the task_id.

        Note: The kernel runs tasks synchronously, so this still blocks
        until the task completes. Use asyncio.create_task() to run
        multiple agents concurrently.

        Returns:
            task_id string.
        """
        result = await self.run(prompt, autonomous=autonomous)
        return result.task_id

    # ------------------------------------------------------------------
    # Tool registration
    # ------------------------------------------------------------------

    async def register_tool(self, fn: Callable[..., Any]) -> None:
        """
        Register a @tool-decorated function with the kernel.

        The tool manifest stored in fn._agentos_manifest is sent as an
        InstallTool command. The kernel writes a temporary TOML manifest file.

        Raises:
            ToolError: If the function is not decorated with @agentos.tool.
        """
        from .exceptions import ToolError

        manifest = getattr(fn, "_agentos_manifest", None)
        if manifest is None:
            raise ToolError(
                f"{fn.__name__} is not decorated with @agentos.tool. "
                "Apply the decorator before calling register_tool()."
            )

        # Kernel InstallTool expects a TOML manifest path.
        # We write a temp file and pass its path.
        import os
        import tempfile

        toml_content = _manifest_to_toml(manifest)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".toml", delete=False, prefix="agentos_tool_"
        ) as f:
            f.write(toml_content)
            tmp_path = f.name

        try:
            await self._client.send_command("InstallTool", manifest_path=tmp_path)
        finally:
            os.unlink(tmp_path)

    # ------------------------------------------------------------------
    # Permissions
    # ------------------------------------------------------------------

    async def grant_permission(self, permission: str) -> None:
        """
        Grant a permission to this agent.

        Args:
            permission: Permission string in "resource:flags" format
                (e.g. "fs.user_data:rw", "process.exec:x").
        """
        await self._client.send_command(
            "GrantPermission",
            agent_name=self._name,
            permission=permission,
        )

    async def revoke_permission(self, permission: str) -> None:
        """Revoke a permission from this agent."""
        await self._client.send_command(
            "RevokePermission",
            agent_name=self._name,
            permission=permission,
        )

    async def show_permissions(self) -> dict[str, Any]:
        """Return the agent's current permission set."""
        response = await self._client.send_command(
            "ShowPermissions",
            agent_name=self._name,
        )
        if isinstance(response, dict) and "Permissions" in response:
            return response["Permissions"]
        raise KernelCommandError(
            f"ShowPermissions returned unexpected response shape: {response}"
        )

    # ------------------------------------------------------------------
    # Task listing
    # ------------------------------------------------------------------

    async def list_tasks(self) -> list[TaskSummary]:
        """Return all tasks known to the kernel."""
        response = await self._client.send_command("ListTasks")
        tasks_raw: list[dict[str, Any]] = []
        if isinstance(response, dict) and "TaskList" in response:
            tasks_raw = response["TaskList"]
        elif isinstance(response, list):
            tasks_raw = response
        return [TaskSummary.from_dict(t) for t in tasks_raw]

    # ------------------------------------------------------------------
    # Notifications
    # ------------------------------------------------------------------

    @property
    def inbox(self) -> Inbox:
        """Access the agent notification inbox."""
        return Inbox(self._client)

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def disconnect(self) -> None:
        """Disconnect this agent from the kernel (marks it Offline)."""
        try:
            await self._client.send_command(
                "DisconnectAgent",
                agent_id=self._agent_id,
            )
        finally:
            await self._client.close()

    async def close(self) -> None:
        """Close the underlying bus connection without notifying the kernel."""
        await self._client.close()

    async def __aenter__(self) -> "Agent":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()

    # ------------------------------------------------------------------
    # Properties
    # ------------------------------------------------------------------

    @property
    def name(self) -> str:
        return self._name

    @property
    def agent_id(self) -> str:
        return self._agent_id

    def __repr__(self) -> str:
        return f"Agent(name={self._name!r}, id={self._agent_id!r})"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _extract_agent_id(response: dict[str, Any]) -> str:
    """
    Extract agent_id from a ConnectAgent success response.

    Wire format: {"Success": {"data": {"agent_id": "..."}}}
    """
    if "Success" in response:
        data = response["Success"].get("data") or {}
        agent_id = data.get("agent_id", "")
        if agent_id:
            return agent_id

    raise KernelCommandError(
        f"ConnectAgent response missing agent_id: {response}"
    )


def _unwrap_success(response: dict[str, Any]) -> dict[str, Any]:
    """
    Unwrap a KernelResponse::Success and return its data dict.

    Wire format: {"Success": {"data": {...}}}
    """
    if "Success" in response:
        data = response["Success"].get("data") or {}
        return data
    raise KernelCommandError(f"Expected Success response, got: {response}")


def _manifest_to_toml(manifest: dict[str, Any]) -> str:
    """
    Convert a @tool manifest dict to a ToolManifest-compatible TOML string.

    The TOML structure must exactly match the ToolManifest serde layout in
    agentos-types/src/tool.rs.  Key serde annotations:
      - TrustTier: rename_all = "lowercase" → "core" / "community" / etc.
      - ExecutorType: rename_all = "lowercase" → "inline" / "wasm"
      - executor.type field: #[serde(rename = "type")] on the executor_type field
    """
    import json

    def q(s: str) -> str:
        """Quote a string as a TOML basic string (JSON quoting is a safe subset)."""
        return json.dumps(s)

    name = q(manifest["name"])
    description = q(manifest["description"])
    version = q(manifest.get("version", "1.0.0"))
    # TrustTier uses rename_all = "lowercase" in serde
    trust_tier = q(manifest.get("trust_tier", "core").lower())
    author = q("python-sdk")

    permissions = manifest.get("permissions", [])
    perm_toml = "[" + ", ".join(q(p) for p in permissions) + "]"

    return (
        f"[manifest]\n"
        f"name = {name}\n"
        f"version = {version}\n"
        f"description = {description}\n"
        f"author = {author}\n"
        f"trust_tier = {trust_tier}\n"
        f"\n"
        f"[capabilities_required]\n"
        f"permissions = {perm_toml}\n"
        f"\n"
        f"[capabilities_provided]\n"
        f"outputs = []\n"
        f"\n"
        f"[intent_schema]\n"
        f'input = "PythonToolInput"\n'
        f'output = "PythonToolOutput"\n'
        f"\n"
        f"[sandbox]\n"
        f"network = false\n"
        f"fs_write = false\n"
        f"gpu = false\n"
        f"max_memory_mb = 64\n"
        f"max_cpu_ms = 5000\n"
        f"\n"
        f"[executor]\n"
        f'type = "inline"\n'
    )
