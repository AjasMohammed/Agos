"""AgentOS exception hierarchy."""


class AgentOSError(Exception):
    """Base exception for all AgentOS SDK errors."""


class KernelConnectionError(AgentOSError):
    """Failed to connect to the kernel socket."""


class KernelCommandError(AgentOSError):
    """Kernel returned an error response to a command."""

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.kernel_message = message


class TaskError(AgentOSError):
    """A task failed or was cancelled."""


class TaskTimeoutError(TaskError):
    """Task did not complete within the timeout."""


class ToolError(AgentOSError):
    """Error registering or executing a tool."""


class NotificationError(AgentOSError):
    """Error interacting with the notification inbox."""
